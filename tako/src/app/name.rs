use std::fs;
use std::path::Path;

use thiserror::Error;

/// Errors that can occur during app name resolution
#[derive(Debug, Error)]
pub enum AppNameError {
    #[error("Could not determine app name: {0}")]
    Resolution(String),

    #[error("App name validation failed: {0}")]
    Validation(String),
}

pub type Result<T> = std::result::Result<T, AppNameError>;

/// Resolve app name from a project directory
///
/// Resolution order:
/// 1. tako.toml `[tako].name` field
/// 2. package.json `name` field (for JS runtimes)
/// 3. go.mod module name (for Go)
/// 4. Directory name as fallback
pub fn resolve_app_name<P: AsRef<Path>>(dir: P) -> Result<String> {
    let dir = dir.as_ref();

    // 1. Check tako.toml
    if let Some(name) = get_name_from_tako_toml(dir) {
        return validate_and_sanitize_app_name(&name);
    }

    // 2. Check package.json
    if let Some(name) = get_name_from_package_json(dir) {
        return validate_and_sanitize_app_name(&name);
    }

    // 3. Check go.mod
    if let Some(name) = get_name_from_go_mod(dir) {
        return validate_and_sanitize_app_name(&name);
    }

    // 4. Fallback to directory name
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            AppNameError::Resolution("Could not determine app name from directory".to_string())
        })?;

    validate_and_sanitize_app_name(&dir_name)
}

/// Get name from tako.toml [tako] section
fn get_name_from_tako_toml<P: AsRef<Path>>(dir: P) -> Option<String> {
    let path = dir.as_ref().join("tako.toml");
    let content = fs::read_to_string(&path).ok()?;

    let toml: toml::Value = toml::from_str(&content).ok()?;
    toml.get("tako")?
        .get("name")?
        .as_str()
        .map(|s| s.to_string())
}

/// Get name from package.json
fn get_name_from_package_json<P: AsRef<Path>>(dir: P) -> Option<String> {
    let path = dir.as_ref().join("package.json");
    let content = fs::read_to_string(&path).ok()?;

    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("name")?.as_str().map(|s| s.to_string())
}

/// Get name from go.mod (module path, extracting last component)
fn get_name_from_go_mod<P: AsRef<Path>>(dir: P) -> Option<String> {
    let path = dir.as_ref().join("go.mod");
    let content = fs::read_to_string(&path).ok()?;

    // Parse "module github.com/user/project" line
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("module ") {
            let module_path = line.trim_start_matches("module ").trim();
            // Get the last component of the module path
            return module_path.rsplit('/').next().map(|s| s.to_string());
        }
    }

    None
}

/// Validate and sanitize app name
///
/// Rules:
/// - 1-63 characters
/// - Lowercase letters, numbers, and hyphens only
/// - Must start with a letter
/// - Cannot end with a hyphen
fn validate_and_sanitize_app_name(name: &str) -> Result<String> {
    // First sanitize
    let sanitized = sanitize_app_name(name);

    // Then validate
    if sanitized.is_empty() {
        return Err(AppNameError::Validation(
            "App name cannot be empty after sanitization".to_string(),
        ));
    }

    if sanitized.len() > 63 {
        return Err(AppNameError::Validation(
            "App name cannot exceed 63 characters".to_string(),
        ));
    }

    // Must start with lowercase letter
    if !sanitized
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase())
        .unwrap_or(false)
    {
        return Err(AppNameError::Validation(
            "App name must start with a lowercase letter".to_string(),
        ));
    }

    // Cannot end with hyphen
    if sanitized.ends_with('-') {
        return Err(AppNameError::Validation(
            "App name cannot end with a hyphen".to_string(),
        ));
    }

    Ok(sanitized)
}

/// Sanitize a string to be a valid app name
///
/// - Converts to lowercase
/// - Replaces underscores and dots with hyphens
/// - Removes invalid characters
/// - Collapses multiple hyphens
/// - Trims leading/trailing hyphens
fn sanitize_app_name(name: &str) -> String {
    let mut result = String::new();

    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            result.push(c.to_ascii_lowercase());
        } else if c == '_' || c == '.' || c == '-' {
            // Replace with hyphen, but avoid consecutive hyphens
            if !result.ends_with('-') {
                result.push('-');
            }
        }
        // Skip other characters
    }

    // Trim leading hyphens and numbers
    while result.starts_with('-') || result.starts_with(|c: char| c.is_ascii_digit()) {
        result.remove(0);
    }

    // Trim trailing hyphens
    while result.ends_with('-') {
        result.pop();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ==================== Resolution Tests ====================

    #[test]
    fn test_resolve_from_tako_toml() {
        let temp_dir = TempDir::new().unwrap();

        let tako_toml = r#"
[tako]
name = "my-app"
"#;
        fs::write(temp_dir.path().join("tako.toml"), tako_toml).unwrap();

        let name = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(name, "my-app");
    }

    #[test]
    fn test_resolve_from_package_json() {
        let temp_dir = TempDir::new().unwrap();

        let package_json = r#"{"name": "my-npm-package"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let name = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(name, "my-npm-package");
    }

    #[test]
    fn test_resolve_from_go_mod() {
        let temp_dir = TempDir::new().unwrap();

        let go_mod = "module github.com/user/my-go-app\n\ngo 1.21\n";
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();

        let name = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(name, "my-go-app");
    }

    #[test]
    fn test_resolve_from_directory_name() {
        let temp_dir = TempDir::new().unwrap();

        // No config files, should use directory name
        let name = resolve_app_name(temp_dir.path()).unwrap();

        // TempDir creates names like ".tmpXXXXX", which get sanitized
        assert!(!name.is_empty());
    }

    #[test]
    fn test_tako_toml_takes_priority() {
        let temp_dir = TempDir::new().unwrap();

        // Create both tako.toml and package.json
        let tako_toml = r#"
[tako]
name = "tako-name"
"#;
        fs::write(temp_dir.path().join("tako.toml"), tako_toml).unwrap();

        let package_json = r#"{"name": "package-name"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let name = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(name, "tako-name");
    }

    #[test]
    fn test_package_json_takes_priority_over_go_mod() {
        let temp_dir = TempDir::new().unwrap();

        // Create both package.json and go.mod
        let package_json = r#"{"name": "package-name"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let go_mod = "module github.com/user/go-name\n";
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();

        let name = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(name, "package-name");
    }

    // ==================== Sanitization Tests ====================

    #[test]
    fn test_sanitize_lowercase() {
        assert_eq!(sanitize_app_name("MyApp"), "myapp");
        assert_eq!(sanitize_app_name("MY-APP"), "my-app");
    }

    #[test]
    fn test_sanitize_underscores_to_hyphens() {
        assert_eq!(sanitize_app_name("my_app"), "my-app");
        assert_eq!(sanitize_app_name("my__app"), "my-app"); // collapses
    }

    #[test]
    fn test_sanitize_dots_to_hyphens() {
        assert_eq!(sanitize_app_name("my.app"), "my-app");
    }

    #[test]
    fn test_sanitize_removes_invalid_chars() {
        assert_eq!(sanitize_app_name("my@app!"), "myapp");
        assert_eq!(sanitize_app_name("my app"), "myapp"); // spaces are removed
    }

    #[test]
    fn test_sanitize_collapses_hyphens() {
        assert_eq!(sanitize_app_name("my--app"), "my-app");
        assert_eq!(sanitize_app_name("my---app"), "my-app");
    }

    #[test]
    fn test_sanitize_trims_hyphens() {
        assert_eq!(sanitize_app_name("-my-app-"), "my-app");
        assert_eq!(sanitize_app_name("---my-app---"), "my-app");
    }

    #[test]
    fn test_sanitize_trims_leading_numbers() {
        assert_eq!(sanitize_app_name("123app"), "app");
        assert_eq!(sanitize_app_name("1-2-3-app"), "app");
    }

    #[test]
    fn test_sanitize_preserves_numbers_in_middle() {
        assert_eq!(sanitize_app_name("app123"), "app123");
        assert_eq!(sanitize_app_name("my-app-2"), "my-app-2");
    }

    // ==================== Validation Tests ====================

    #[test]
    fn test_validate_valid_names() {
        assert!(validate_and_sanitize_app_name("my-app").is_ok());
        assert!(validate_and_sanitize_app_name("api").is_ok());
        assert!(validate_and_sanitize_app_name("my-app-123").is_ok());
        assert!(validate_and_sanitize_app_name("a").is_ok());
    }

    #[test]
    fn test_validate_empty_after_sanitization() {
        // All invalid characters
        assert!(validate_and_sanitize_app_name("@#$%").is_err());
        // Only numbers and hyphens
        assert!(validate_and_sanitize_app_name("123-456").is_err());
    }

    #[test]
    fn test_validate_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_and_sanitize_app_name(&long_name).is_err());

        let ok_name = "a".repeat(63);
        assert!(validate_and_sanitize_app_name(&ok_name).is_ok());
    }

    #[test]
    fn test_validate_must_start_with_letter() {
        // After sanitization, leading numbers are stripped
        assert_eq!(validate_and_sanitize_app_name("123app").unwrap(), "app");
    }

    #[test]
    fn test_validate_cannot_end_with_hyphen() {
        // After sanitization, trailing hyphens are stripped
        assert_eq!(validate_and_sanitize_app_name("my-app-").unwrap(), "my-app");
    }

    // ==================== go.mod Parsing Tests ====================

    #[test]
    fn test_parse_go_mod_simple() {
        let temp_dir = TempDir::new().unwrap();

        let go_mod = "module myapp\n\ngo 1.21\n";
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();

        let name = get_name_from_go_mod(temp_dir.path()).unwrap();
        assert_eq!(name, "myapp");
    }

    #[test]
    fn test_parse_go_mod_with_path() {
        let temp_dir = TempDir::new().unwrap();

        let go_mod = "module github.com/organization/project\n\ngo 1.21\n";
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();

        let name = get_name_from_go_mod(temp_dir.path()).unwrap();
        assert_eq!(name, "project");
    }

    #[test]
    fn test_parse_go_mod_deeply_nested() {
        let temp_dir = TempDir::new().unwrap();

        let go_mod = "module github.com/org/repo/cmd/api\n";
        fs::write(temp_dir.path().join("go.mod"), go_mod).unwrap();

        let name = get_name_from_go_mod(temp_dir.path()).unwrap();
        assert_eq!(name, "api");
    }

    // ==================== package.json Parsing Tests ====================

    #[test]
    fn test_parse_package_json_simple() {
        let temp_dir = TempDir::new().unwrap();

        let package_json = r#"{"name": "simple-app"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let name = get_name_from_package_json(temp_dir.path()).unwrap();
        assert_eq!(name, "simple-app");
    }

    #[test]
    fn test_parse_package_json_scoped() {
        let temp_dir = TempDir::new().unwrap();

        let package_json = r#"{"name": "@org/my-package"}"#;
        fs::write(temp_dir.path().join("package.json"), package_json).unwrap();

        let name = get_name_from_package_json(temp_dir.path()).unwrap();
        assert_eq!(name, "@org/my-package");

        // When resolved, @ and / are stripped (not alphanumeric)
        let resolved = resolve_app_name(temp_dir.path()).unwrap();
        assert_eq!(resolved, "orgmy-package");
    }
}

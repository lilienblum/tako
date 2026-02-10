mod bun;

use std::collections::HashMap;
use std::path::Path;

pub use bun::BunRuntime;

/// Runtime mode (development or production)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    Development,
    Production,
}

/// Trait for runtime adapters (Bun, Node, Deno, Go, etc.)
pub trait RuntimeAdapter: Send + Sync {
    /// Runtime name (e.g., "bun", "node", "deno")
    fn name(&self) -> &str;

    /// Get runtime version
    fn version(&self) -> Option<String>;

    /// Get the entry point file path
    fn entry_point(&self) -> &Path;

    /// Get the build command (if any)
    fn build_command(&self) -> Option<Vec<String>>;

    /// Get the command to run the app with a specific port
    fn run_command(&self, port: u16) -> Vec<String>;

    /// Get environment variables to set based on mode
    fn env_vars(&self, mode: RuntimeMode) -> HashMap<String, String>;
}

/// Detect runtime from a directory
pub fn detect_runtime<P: AsRef<Path>>(dir: P) -> Option<Box<dyn RuntimeAdapter>> {
    let dir = dir.as_ref();

    // Try Bun first
    if let Some(bun) = BunRuntime::detect(dir) {
        return Some(Box::new(bun));
    }

    // Add more runtimes here in the future:
    // - Node
    // - Deno
    // - Go
    // - etc.

    None
}

/// Get a runtime adapter by name
pub fn get_runtime<P: AsRef<Path>>(name: &str, dir: P) -> Option<Box<dyn RuntimeAdapter>> {
    let dir = dir.as_ref();

    match name.to_lowercase().as_str() {
        "bun" => BunRuntime::detect(dir).map(|r| Box::new(r) as Box<dyn RuntimeAdapter>),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    struct FakeRuntimeDefault;

    impl RuntimeAdapter for FakeRuntimeDefault {
        fn name(&self) -> &str {
            "fake-default"
        }

        fn version(&self) -> Option<String> {
            Some("0.0.0".to_string())
        }

        fn entry_point(&self) -> &Path {
            Path::new("index.ts")
        }

        fn build_command(&self) -> Option<Vec<String>> {
            None
        }

        fn run_command(&self, _port: u16) -> Vec<String> {
            vec!["fake".to_string()]
        }

        fn env_vars(&self, _mode: RuntimeMode) -> HashMap<String, String> {
            HashMap::new()
        }
    }

    #[test]
    fn test_trait_is_object_safe() {
        // This test verifies that RuntimeAdapter can be used as a trait object
        fn accept_runtime(_: &dyn RuntimeAdapter) {}

        // If this compiles, the trait is object-safe
        let _ = accept_runtime;
    }

    #[test]
    fn test_runtime_mode_equality() {
        assert_eq!(RuntimeMode::Development, RuntimeMode::Development);
        assert_eq!(RuntimeMode::Production, RuntimeMode::Production);
        assert_ne!(RuntimeMode::Development, RuntimeMode::Production);
    }

    #[test]
    fn test_minimal_runtime_impl_compiles() {
        let runtime = FakeRuntimeDefault;
        assert_eq!(runtime.name(), "fake-default");
    }
}

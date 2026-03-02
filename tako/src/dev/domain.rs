//! Tako Dev Domain Utilities
//!
//! Shared constants and helpers for development domains.

/// Tako development domain TLD
pub const TAKO_DEV_DOMAIN: &str = "tako";

/// Get the Tako dev domain for an app
///
/// # Example
/// ```
/// use crate::local_dev::get_tako_domain;
/// assert_eq!(get_tako_domain("my-app"), "my-app.tako");
/// ```
pub fn get_tako_domain(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_DEV_DOMAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_tako_domain() {
        assert_eq!(get_tako_domain("my-app"), "my-app.tako");
        assert_eq!(get_tako_domain("api-server"), "api-server.tako");
    }
}

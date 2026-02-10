//! Tako Local Domain Utilities
//!
//! Shared constants and helpers for local development domains.

/// Tako local development domain suffix
pub const TAKO_LOCAL_DOMAIN: &str = "tako.local";

/// Get the Tako local domain for an app
///
/// # Example
/// ```
/// use crate::local_dev::get_tako_domain;
/// assert_eq!(get_tako_domain("my-app"), "my-app.tako.local");
/// ```
pub fn get_tako_domain(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_LOCAL_DOMAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_tako_domain() {
        assert_eq!(get_tako_domain("my-app"), "my-app.tako.local");
        assert_eq!(get_tako_domain("api-server"), "api-server.tako.local");
    }
}

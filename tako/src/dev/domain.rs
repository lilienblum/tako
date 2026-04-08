//! Tako Dev Domain Utilities
//!
//! Shared constants and helpers for development domains.

/// Tako development domain (scoped, always available)
pub const TAKO_DEV_DOMAIN: &str = "tako.test";

/// Short development domain (preferred when `/etc/resolver/test` is available)
pub const SHORT_DEV_DOMAIN: &str = "test";

/// Get the full Tako dev domain for an app (`{app}.tako.test`)
pub fn get_tako_domain(app_name: &str) -> String {
    format!("{}.{}", app_name, TAKO_DEV_DOMAIN)
}

/// Get the short dev domain for an app (`{app}.test`)
pub fn get_short_domain(app_name: &str) -> String {
    format!("{}.{}", app_name, SHORT_DEV_DOMAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_tako_domain() {
        assert_eq!(get_tako_domain("my-app"), "my-app.tako.test");
        assert_eq!(get_tako_domain("api-server"), "api-server.tako.test");
    }

    #[test]
    fn test_get_short_domain() {
        assert_eq!(get_short_domain("my-app"), "my-app.test");
        assert_eq!(get_short_domain("api-server"), "api-server.test");
    }
}

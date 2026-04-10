pub(crate) fn split_route_pattern(route: &str) -> (&str, Option<&str>) {
    match route.find('/') {
        Some(idx) => (&route[..idx], Some(&route[idx..])),
        None => (route, None),
    }
}

pub(crate) fn to_local_hostname(hostname: &str) -> Option<String> {
    let (wildcard, host) = if let Some(rest) = hostname.strip_prefix("*.") {
        (true, rest)
    } else {
        (false, hostname)
    };

    let base = host
        .strip_suffix(".tako.test")
        .or_else(|| host.strip_suffix(".test"))
        .or_else(|| host.strip_suffix(".local"))?;
    let local = format!("{base}.local");

    if wildcard {
        Some(format!("*.{local}"))
    } else {
        Some(local)
    }
}

#[allow(dead_code)]
pub(crate) fn to_local_route(route: &str) -> Option<String> {
    let (host, path) = split_route_pattern(route);
    let local_host = to_local_hostname(host)?;
    Some(match path {
        Some(path) => format!("{local_host}{path}"),
        None => local_host,
    })
}

pub(crate) fn route_host_matches_request(route_host: &str, request_host: &str) -> bool {
    host_pattern_matches(route_host, request_host)
        || to_local_hostname(route_host)
            .is_some_and(|local_host| host_pattern_matches(&local_host, request_host))
}

fn host_pattern_matches(pattern: &str, hostname: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        if hostname == suffix {
            return false;
        }
        hostname.len() > suffix.len()
            && hostname.as_bytes()[hostname.len() - suffix.len() - 1] == b'.'
            && hostname.ends_with(suffix)
    } else {
        pattern == hostname
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_local_hostname_rewrites_test_suffixes() {
        assert_eq!(to_local_hostname("app.test").as_deref(), Some("app.local"));
        assert_eq!(
            to_local_hostname("app.tako.test").as_deref(),
            Some("app.local")
        );
        assert_eq!(
            to_local_hostname("*.app.tako.test").as_deref(),
            Some("*.app.local")
        );
    }

    #[test]
    fn to_local_route_preserves_path_suffix() {
        assert_eq!(
            to_local_route("app.tako.test/api/*").as_deref(),
            Some("app.local/api/*")
        );
    }

    #[test]
    fn route_host_matches_request_accepts_local_aliases() {
        assert!(route_host_matches_request("app.tako.test", "app.local"));
        assert!(route_host_matches_request(
            "*.app.tako.test",
            "foo.app.local"
        ));
        assert!(!route_host_matches_request("app.tako.test", "other.local"));
    }
}

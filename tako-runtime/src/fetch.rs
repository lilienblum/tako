use base64::Engine;

/// Fallback repo slug when `CARGO_PKG_REPOSITORY` cannot be parsed.
const FALLBACK_REPO: &str = "lilienblum/tako";

/// Repository URL from workspace Cargo.toml (resolved at compile time).
const PACKAGE_REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// Branch to fetch runtime definitions from.
pub const OFFICIAL_BRANCH: &str = "master";

/// Derive the `owner/repo` slug from the workspace package repository URL.
pub fn official_repo() -> String {
    parse_github_repo_slug(PACKAGE_REPOSITORY_URL).unwrap_or_else(|| FALLBACK_REPO.to_string())
}

fn parse_github_repo_slug(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Handle "owner/repo" directly
    if !trimmed.contains(':') && !trimmed.contains('/') {
        return None;
    }
    if !trimmed.contains(':') && trimmed.matches('/').count() == 1 {
        return Some(trimmed.to_string());
    }

    // Handle SSH: git@github.com:owner/repo.git
    if let Some((_prefix, path)) = trimmed.split_once(':')
        && !path.contains("//")
    {
        let (owner, rest) = path.split_once('/')?;
        let repo = rest.strip_suffix(".git").unwrap_or(rest).trim();
        if repo.is_empty() {
            return None;
        }
        return Some(format!("{owner}/{repo}"));
    }

    // Handle HTTPS: https://github.com/owner/repo[.git]
    let path = trimmed.split("//").nth(1)?;
    let mut segments = path.split('/').skip(1); // skip hostname
    let owner = segments.next()?;
    let repo = segments.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo).trim();
    if repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Fetch a runtime TOML file from the official GitHub repository.
///
/// Uses the GitHub Contents API to retrieve the raw file content.
/// Returns the file content as a string.
pub async fn fetch_runtime_toml(id: &str) -> Result<String, String> {
    let repo = official_repo();
    let branch = OFFICIAL_BRANCH;
    let path = format!("runtimes/{id}.toml");
    let url = format!("https://api.github.com/repos/{repo}/contents/{path}?ref={branch}");

    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("failed to fetch runtime '{id}': {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "failed to fetch runtime '{id}': HTTP {}",
            response.status()
        ));
    }

    let raw = response
        .text()
        .await
        .map_err(|e| format!("failed to read response for runtime '{id}': {e}"))?;

    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("invalid JSON for runtime '{id}': {e}"))?;

    let content_b64 = json
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing 'content' field for runtime '{id}'"))?;

    let normalized = content_b64.replace('\n', "");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(normalized)
        .map_err(|e| format!("failed to decode runtime '{id}' content: {e}"))?;

    String::from_utf8(bytes).map_err(|e| format!("invalid UTF-8 for runtime '{id}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_repo_resolves_from_cargo_pkg() {
        let repo = official_repo();
        assert_eq!(repo, "lilienblum/tako");
    }

    #[test]
    fn parse_github_repo_slug_handles_https() {
        assert_eq!(
            parse_github_repo_slug("https://github.com/lilienblum/tako"),
            Some("lilienblum/tako".to_string())
        );
    }

    #[test]
    fn parse_github_repo_slug_handles_git_suffix() {
        assert_eq!(
            parse_github_repo_slug("https://github.com/lilienblum/tako.git"),
            Some("lilienblum/tako".to_string())
        );
    }

    #[test]
    fn parse_github_repo_slug_handles_ssh() {
        assert_eq!(
            parse_github_repo_slug("git@github.com:lilienblum/tako.git"),
            Some("lilienblum/tako".to_string())
        );
    }

    #[test]
    fn parse_github_repo_slug_handles_bare_slug() {
        assert_eq!(
            parse_github_repo_slug("lilienblum/tako"),
            Some("lilienblum/tako".to_string())
        );
    }
}

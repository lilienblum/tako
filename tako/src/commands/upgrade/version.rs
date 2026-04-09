use crate::config::UpgradeChannel;
use crate::output;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const CANARY_SHA: Option<&str> = option_env!("TAKO_CANARY_SHA");

const REPO_OWNER: &str = "lilienblum";
const REPO_NAME: &str = "tako";
const TAGS_API: &str = "https://api.github.com/repos/lilienblum/tako/tags?per_page=100";
const TAG_PREFIX: &str = "tako-v";

pub(super) enum UpdateCheck {
    AlreadyCurrent,
    Available { tag: String, version: String },
}

pub(super) fn current_version() -> String {
    match CANARY_SHA {
        Some(sha) if !sha.trim().is_empty() => {
            let short = &sha.trim()[..sha.trim().len().min(7)];
            format!("canary-{short}")
        }
        _ => CURRENT_VERSION.to_string(),
    }
}

pub(super) async fn fetch_canary_version() -> Result<String, Box<dyn std::error::Error>> {
    let url =
        format!("https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/git/ref/tags/canary-latest");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("User-Agent", "tako-cli")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| format!("failed to resolve canary-latest tag: {e}"))?;
    let body: serde_json::Value = resp.json().await?;
    let sha = body["object"]["sha"]
        .as_str()
        .ok_or("canary-latest tag response missing object.sha")?;
    let short = &sha[..sha.len().min(7)];
    Ok(format!("canary-{short}"))
}

pub(super) fn tarball_url_for_tag(tag: &str, os: &str, arch: &str) -> String {
    if let Ok(base) = std::env::var("TAKO_DOWNLOAD_BASE_URL") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            if !base.starts_with("https://") {
                output::warning(&format!(
                    "TAKO_DOWNLOAD_BASE_URL uses non-HTTPS scheme — binary will be downloaded over an insecure connection: {base}"
                ));
            }
            return format!("{base}/tako-{os}-{arch}.tar.gz");
        }
    }
    format!(
        "https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{tag}/tako-{os}-{arch}.tar.gz"
    )
}

pub(super) async fn check_for_updates(channel: UpgradeChannel) -> Result<UpdateCheck, String> {
    output::with_spinner_async_simple("Checking for updates", check_for_updates_inner(channel))
        .await
}

pub(super) async fn check_for_updates_inner(
    channel: UpgradeChannel,
) -> Result<UpdateCheck, String> {
    tracing::debug!("Fetching tags from {}…", TAGS_API);
    let _t = output::timed("Fetch version tags");
    let tags = fetch_tags().await?;
    tracing::debug!("Fetched {} tag(s)", tags.len());

    assert!(channel == UpgradeChannel::Stable);

    let tag = tags
        .iter()
        .find(|tag| tag.name.starts_with(TAG_PREFIX))
        .ok_or_else(|| format!("no release found with prefix '{TAG_PREFIX}'"))?;

    let version = tag.name.strip_prefix(TAG_PREFIX).unwrap_or(&tag.name);
    tracing::debug!("Current: {}, latest: {}", CURRENT_VERSION, version);
    if version == CURRENT_VERSION {
        Ok(UpdateCheck::AlreadyCurrent)
    } else {
        Ok(UpdateCheck::Available {
            tag: tag.name.clone(),
            version: version.to_string(),
        })
    }
}

#[derive(Debug)]
struct TagInfo {
    name: String,
}

async fn fetch_tags() -> Result<Vec<TagInfo>, String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(TAGS_API)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("GitHub API returned {}", resp.status()));
    }

    let text = resp
        .text()
        .await
        .map_err(|e| format!("failed to read response: {e}"))?;

    let raw: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| format!("failed to parse tags: {e}"))?;

    let mut tags = Vec::new();
    for entry in &raw {
        if let Some(name) = entry["name"].as_str() {
            tags.push(TagInfo {
                name: name.to_string(),
            });
        }
    }

    Ok(tags)
}

pub(super) async fn fetch_latest_stable_tag() -> Result<String, String> {
    let tags = fetch_tags().await?;
    tags.iter()
        .find(|tag| tag.name.starts_with(TAG_PREFIX))
        .map(|tag| tag.name.clone())
        .ok_or_else(|| format!("no release found with prefix '{TAG_PREFIX}'"))
}

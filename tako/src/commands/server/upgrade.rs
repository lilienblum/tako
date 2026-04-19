use crate::output;
use crate::ssh::SshClient;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tako_core::ServerRuntimeInfo;
use tracing::Instrument;

pub(super) const UPGRADE_SOCKET_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
const UPGRADE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const SERVER_BINARY_PATH: &str = "/usr/local/bin/tako-server";
const SERVER_PREVIOUS_BINARY_PATH: &str = "/usr/local/bin/tako-server.prev";

const REPO_OWNER: &str = "lilienblum";
const REPO_NAME: &str = "tako";
const LATEST_TAG: &str = "latest";
const SERVER_CHECKSUM_MANIFEST_ASSET: &str = "tako-server-sha256s.txt";
const SERVER_CHECKSUM_SIGNATURE_ASSET: &str = "tako-server-sha256s.txt.sig";
const ALLOW_INSECURE_DOWNLOAD_BASE_ENV: &str = "TAKO_ALLOW_INSECURE_DOWNLOAD_BASE";
const SERVER_RELEASE_SIGNING_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAuSti08sNCTG7S1oGDSB3\n\
vThbzAfQQzGq+wQjVkjN1VEPFk21eWqYMEAN2jU3FhTZDrsfl5iEMv1NsE6bimjd\n\
LN3UtdvqnxdF08wlCmbu4tO7thJE4CNY1uY4qHjI1aqBSozJ92x8vkel1DZKUxG0\n\
aK1YdrP0bqbuikK8f5wFgMGPO0sfSH5FKH7N0SseEoMZt1bGh7bL8G2EEDo91uEb\n\
w0OcbZGhZ/G3Kbv9dBQAS16eEgH/d0ssruPjdsQbFD+hnywgiqC8lOro1cmr1bBN\n\
d+Q7l60r6e3Y4kmH3OCqRzmIcKnv+6Piot9YHqMxptd6BuiE6x72w9j2loOLnB5j\n\
ytknLq3YykchWrbwLYqVspjN6FcqPZgI6bIEhsaFLRD6tjTqYBmEHcpLk//26p7a\n\
1/r22DyKdHO3/GS0L2sYVKkD/7R9N5QfnRd3erbx7je0pzDDe/x31h4X7vGgjCTy\n\
xm4tDiIHBg92bd3+ag9qnvulBH1uEb2i+grxFYefUkKpAgMBAAE=\n\
-----END PUBLIC KEY-----\n";

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedReleaseAsset {
    download_url: String,
    expected_sha256: String,
}

fn build_upgrade_owner(server_name: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let raw = format!("upgrade-{server_name}-{now}-{}", std::process::id());
    raw.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

fn server_binary_archive_name(target: &crate::config::ServerTarget) -> String {
    format!("tako-server-linux-{}-{}.tar.zst", target.arch, target.libc)
}

fn parse_boolish_env(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn allow_insecure_download_base() -> bool {
    std::env::var(ALLOW_INSECURE_DOWNLOAD_BASE_ENV)
        .map(|value| parse_boolish_env(&value))
        .unwrap_or(false)
}

fn validate_download_base(base: &str, allow_insecure: bool) -> Result<(), String> {
    if base.starts_with("https://") {
        return Ok(());
    }
    if allow_insecure {
        output::warning(&format!(
            "Using insecure download base '{}'; this is intended only for local testing.",
            base
        ));
        return Ok(());
    }
    Err(format!(
        "TAKO_DOWNLOAD_BASE_URL must use https://. Set {ALLOW_INSECURE_DOWNLOAD_BASE_ENV}=1 to allow an insecure override for local testing."
    ))
}

fn server_download_base(custom_base: Option<&str>, allow_insecure: bool) -> Result<String, String> {
    let base = if let Some(raw) = custom_base {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            default_download_base()
        } else {
            validate_download_base(trimmed, allow_insecure)?;
            trimmed.to_string()
        }
    } else if let Ok(env_base) = std::env::var("TAKO_DOWNLOAD_BASE_URL") {
        let trimmed = env_base.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            default_download_base()
        } else {
            validate_download_base(trimmed, allow_insecure)?;
            trimmed.to_string()
        }
    } else {
        default_download_base()
    };
    Ok(base)
}

fn server_binary_download_url(
    target: &crate::config::ServerTarget,
    custom_base: Option<&str>,
    allow_insecure: bool,
) -> Result<String, String> {
    let base = server_download_base(custom_base, allow_insecure)?;
    Ok(format!("{}/{}", base, server_binary_archive_name(target)))
}

fn default_download_base() -> String {
    format!("https://github.com/{REPO_OWNER}/{REPO_NAME}/releases/download/{LATEST_TAG}")
}

fn parse_sha256_manifest_value(manifest: &str, filename: &str) -> Result<String, String> {
    for line in manifest
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let mut parts = line.split_whitespace();
        let Some(hash) = parts.next() else {
            continue;
        };
        let Some(name) = parts.next() else {
            continue;
        };
        let normalized_name = name.trim_start_matches('*').trim_start_matches("./");
        if normalized_name == filename {
            if hash.len() == 64 && hash.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return Ok(hash.to_ascii_lowercase());
            }
            return Err(format!(
                "checksum manifest entry for '{filename}' contains an invalid SHA-256 value"
            ));
        }
    }
    Err(format!("checksum manifest missing entry for '{filename}'"))
}

fn verify_signed_server_checksum_manifest(manifest: &[u8], signature: &[u8]) -> Result<(), String> {
    let key =
        openssl::pkey::PKey::public_key_from_pem(SERVER_RELEASE_SIGNING_PUBLIC_KEY_PEM.as_bytes())
            .map_err(|e| format!("failed to load embedded server release public key: {e}"))?;
    let mut verifier =
        openssl::sign::Verifier::new(openssl::hash::MessageDigest::sha256(), &key)
            .map_err(|e| format!("failed to initialize server release signature verifier: {e}"))?;
    verifier
        .update(manifest)
        .map_err(|e| format!("failed to hash server release checksum manifest: {e}"))?;
    let verified = verifier
        .verify(signature)
        .map_err(|e| format!("failed to verify server checksum signature: {e}"))?;
    if verified {
        Ok(())
    } else {
        Err("server checksum signature verification failed".to_string())
    }
}

async fn fetch_release_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", "tako-cli")
        .send()
        .await
        .map_err(|e| format!("request failed for {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "download failed for {url}: HTTP {}",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| format!("failed to read response body from {url}: {e}"))
}

async fn resolve_verified_server_release_asset(
    target: &crate::config::ServerTarget,
) -> Result<VerifiedReleaseAsset, String> {
    let allow_insecure = allow_insecure_download_base();
    let custom_base = std::env::var("TAKO_DOWNLOAD_BASE_URL").ok();
    let custom_base_ref = custom_base.as_deref();
    let base = server_download_base(custom_base_ref, allow_insecure)?;
    let is_custom_source = custom_base_ref
        .map(|b| !b.trim().is_empty())
        .unwrap_or(false);
    let archive_name = server_binary_archive_name(target);
    let download_url = server_binary_download_url(target, custom_base_ref, allow_insecure)?;
    let manifest_url = format!("{base}/{SERVER_CHECKSUM_MANIFEST_ASSET}");
    let manifest = fetch_release_bytes(&manifest_url).await?;
    if is_custom_source {
        output::warning(
            "Skipping release signature verification because TAKO_DOWNLOAD_BASE_URL is set. \
             Checksums will still be verified after download.",
        );
    } else {
        let signature_url = format!("{base}/{SERVER_CHECKSUM_SIGNATURE_ASSET}");
        let signature = fetch_release_bytes(&signature_url).await?;
        verify_signed_server_checksum_manifest(&manifest, &signature)?;
    }
    let manifest_text = std::str::from_utf8(&manifest)
        .map_err(|e| format!("signed checksum manifest was not valid UTF-8: {e}"))?;
    let expected_sha256 = parse_sha256_manifest_value(manifest_text, &archive_name)?;
    Ok(VerifiedReleaseAsset {
        download_url,
        expected_sha256,
    })
}

fn verify_downloaded_sha256_script(path_expr: &str, expected_sha256: &str) -> String {
    let expected_sha256 = crate::shell::shell_single_quote(expected_sha256);
    format!(
        "expected_sha={expected_sha256}; \
         actual_sha=''; \
         if command -v sha256sum >/dev/null 2>&1; then \
           actual_sha=$(sha256sum {path_expr} | awk '{{print $1}}'); \
         elif command -v shasum >/dev/null 2>&1; then \
           actual_sha=$(shasum -a 256 {path_expr} | awk '{{print $1}}'); \
         elif command -v openssl >/dev/null 2>&1; then \
           actual_sha=$(openssl dgst -sha256 {path_expr} | awk '{{print $NF}}'); \
         else \
           echo 'error: sha256 tool not found' >&2; exit 1; \
         fi; \
         if [ \"$actual_sha\" != \"$expected_sha\" ]; then \
           echo \"error: sha256 mismatch (expected=$expected_sha actual=$actual_sha)\" >&2; exit 1; \
         fi"
    )
}

fn remote_binary_replace_command(url: &str, expected_sha256: &str) -> String {
    use crate::shell::shell_single_quote;
    let url_q = shell_single_quote(url);
    let sha_check = verify_downloaded_sha256_script("\"$archive\"", expected_sha256);
    let script = format!(
        "set -eu; \
         tmp=$(mktemp -d); \
         archive=\"$tmp/tako-server.tar.zst\"; \
         trap 'rm -rf \"$tmp\"' EXIT; \
         curl -fsSL {url_q} -o \"$archive\"; \
         {sha_check}; \
         zstd -d \"$archive\" --stdout | tar -x -C \"$tmp\"; \
         bin=$(find \"$tmp\" -type f -name tako-server | head -n 1); \
         if [ -z \"$bin\" ]; then echo 'error: archive did not contain tako-server binary' >&2; exit 1; fi; \
         if [ -f {SERVER_BINARY_PATH} ]; then install -m 0755 {SERVER_BINARY_PATH} {SERVER_PREVIOUS_BINARY_PATH}; fi; \
         install -m 0755 \"$bin\" {SERVER_BINARY_PATH}; \
         if command -v setcap >/dev/null 2>&1; then setcap cap_net_bind_service=+ep {SERVER_BINARY_PATH} 2>/dev/null || true; fi"
    );
    SshClient::run_with_root_or_sudo(&script)
}

fn remote_restore_previous_binary_command() -> String {
    let script = format!(
        "set -eu; \
         if [ ! -f {SERVER_PREVIOUS_BINARY_PATH} ]; then echo 'error: previous tako-server binary not found' >&2; exit 1; fi; \
         install -m 0755 {SERVER_PREVIOUS_BINARY_PATH} {SERVER_BINARY_PATH}; \
         if command -v setcap >/dev/null 2>&1; then setcap cap_net_bind_service=+ep {SERVER_BINARY_PATH} 2>/dev/null || true; fi"
    );
    SshClient::run_with_root_or_sudo(&script)
}

fn remote_cleanup_previous_binary_command() -> String {
    SshClient::run_with_root_or_sudo(&format!("rm -f {SERVER_PREVIOUS_BINARY_PATH}"))
}

pub(super) async fn wait_for_primary_ready(
    ssh: &mut crate::ssh::SshClient,
    timeout: Duration,
    old_pid: u32,
    server_name: &str,
) -> Result<ServerRuntimeInfo, String> {
    let start = std::time::Instant::now();
    let mut last_err = String::new();
    let mut last_seen_pid: Option<u32> = None;
    let mut poll_count = 0u32;
    while start.elapsed() < timeout {
        ssh.clear_tako_hello_cache();
        poll_count += 1;
        match ssh.tako_server_info().await {
            Ok(info) if info.pid != old_pid => {
                tracing::debug!(
                    server = server_name,
                    new_pid = info.pid,
                    old_pid,
                    polls = poll_count,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "new server process detected"
                );
                return Ok(info);
            }
            Ok(info) => {
                last_seen_pid = Some(info.pid);
                tracing::debug!(
                    server = server_name,
                    pid = info.pid,
                    polls = poll_count,
                    "still seeing old PID, waiting"
                );
                tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            }
            Err(e) => {
                last_err = e.to_string();
                tracing::debug!(
                    server = server_name,
                    error = %e,
                    polls = poll_count,
                    "socket probe failed, waiting"
                );
                tokio::time::sleep(UPGRADE_POLL_INTERVAL).await;
            }
        }
    }

    let service_status = match ssh.tako_status().await {
        Ok(s) => s,
        Err(_) => "unknown".to_string(),
    };

    let detail = if !last_err.is_empty() {
        format!("last socket error: {last_err}")
    } else if let Some(pid) = last_seen_pid {
        format!("socket still reports old pid {pid}")
    } else {
        "no response received".to_string()
    };

    Err(format!(
        "timed out after {:.0}s waiting for new server process (old pid {old_pid}): {detail}; service status: {service_status}",
        timeout.as_secs_f64(),
    ))
}

pub(super) async fn upgrade_servers(name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::ServersToml;

    let servers = ServersToml::load()?;
    if servers.is_empty() {
        output::error("No servers configured.");
        output::hint(&format!(
            "Run {} to add a server.",
            output::strong("tako servers add")
        ));
        return Ok(());
    }

    let names: Vec<String> = if let Some(name) = name {
        if !servers.contains(name) {
            return Err(format!("Server '{}' not found.", name).into());
        }
        vec![name.to_string()]
    } else {
        let mut names: Vec<String> = servers.names().iter().map(|s| s.to_string()).collect();
        names.sort_unstable();
        names
    };

    let interactive = output::is_pretty() && output::is_interactive();

    // CLI and server ship together; the CLI's version is the latest.
    let latest_version = crate::cli::display_version();
    tracing::info!("Latest version: {latest_version}");
    output::info(&format!(
        "Latest version: {}",
        output::strong(&latest_version)
    ));

    // ── Phase 1: Get current versions from all servers ──────────────
    let total = names.len();
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    struct VersionCheck {
        name: String,
        ssh: Option<SshClient>,
        version: Option<String>,
        target: Option<crate::config::ServerTarget>,
        error: Option<String>,
        elapsed: Duration,
    }

    let mut version_set = tokio::task::JoinSet::new();
    for server_name in &names {
        let server = servers
            .get(server_name)
            .ok_or_else(|| format!("Server '{}' not found.", server_name))?
            .clone();
        let name = server_name.clone();
        let done = Arc::clone(&done);
        let span = output::scope(&name);
        version_set.spawn(
            async move {
                let start = std::time::Instant::now();
                let ssh = match SshClient::connect_to(&server.host, server.port).await {
                    Ok(ssh) => ssh,
                    Err(e) => {
                        done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return VersionCheck {
                            name,
                            ssh: None,
                            version: None,
                            target: None,
                            error: Some(e.to_string()),
                            elapsed: start.elapsed(),
                        };
                    }
                };
                let version = ssh.tako_version().await.ok().flatten();
                let target = super::wizard::detect_server_target(&ssh).await.ok();
                done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                VersionCheck {
                    name,
                    ssh: Some(ssh),
                    version,
                    target,
                    error: None,
                    elapsed: start.elapsed(),
                }
            }
            .instrument(span),
        );
    }

    let pb = if interactive {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_style(output::phase_spinner_style());
        let msg = if total == 1 {
            format!("Getting current version for {}…", output::strong(&names[0]))
        } else {
            format!(
                "Getting current versions… {}",
                output::muted_progress(0, total)
            )
        };
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(80));
        Some(pb)
    } else {
        if total == 1 {
            tracing::info!("Getting current version for {}…", &names[0]);
        } else {
            tracing::info!("Getting current versions for {} servers…", total);
        }
        None
    };

    let mut checks: Vec<VersionCheck> = Vec::new();
    while let Some(join_result) = version_set.join_next().await {
        let check = match join_result {
            Ok(v) => v,
            Err(e) => {
                if let Some(ref pb) = pb {
                    pb.finish_and_clear();
                }
                return Err(e.to_string().into());
            }
        };
        if let Some(ref pb) = pb {
            let finished = done.load(std::sync::atomic::Ordering::Relaxed);
            if total > 1 {
                pb.set_message(format!(
                    "Getting current versions… {}",
                    output::muted_progress(finished, total)
                ));
            }
        }
        if let Some(ref v) = check.version {
            let _scope = output::scope(&check.name).entered();
            let time = output::format_elapsed_trace(check.elapsed);
            tracing::debug!("Current version: {v} {time}");
        }
        checks.push(check);
    }

    if let Some(ref pb) = pb {
        pb.finish_and_clear();
    }
    checks.sort_by(|a, b| {
        let pos_a = names
            .iter()
            .position(|n| n == &a.name)
            .unwrap_or(usize::MAX);
        let pos_b = names
            .iter()
            .position(|n| n == &b.name)
            .unwrap_or(usize::MAX);
        pos_a.cmp(&pos_b)
    });

    // ── Phase 2: Per-server upgrade ─────────────────────────────────
    let mut has_error = false;
    for (i, mut check) in checks.into_iter().enumerate() {
        if i > 0 {
            output::heading(&format!("Server {}", output::strong(&check.name)));
        } else {
            output::heading_no_gap(&format!("Server {}", output::strong(&check.name)));
        }

        let _upgrade_scope = output::scope(&check.name).entered();
        let current_ver = check.version.as_deref().unwrap_or("unknown");

        if let Some(ref err) = check.error {
            output::error(err);
            has_error = true;
            continue;
        }

        if check.version.as_deref() == Some(latest_version.as_str()) {
            output::success(&format!("Already on latest build ({current_ver})"));
            if let Some(mut ssh) = check.ssh.take() {
                let _ = ssh.disconnect().await;
            }
            continue;
        }

        let mut ssh = check.ssh.take().unwrap();
        let spinner = output::PhaseSpinner::start_indented(&format!("Upgrading to {current_ver}…"));

        let target = match check.target {
            Some(t) => t,
            None => {
                has_error = true;
                spinner.finish_err_indented("Could not detect server target");
                let _ = ssh.disconnect().await;
                continue;
            }
        };
        match run_server_upgrade(&check.name, &mut ssh, check.version.as_deref(), &target).await {
            Ok(version_after) => {
                let ver = version_after.as_deref().unwrap_or("unknown");
                if ver == current_ver {
                    spinner.finish_ok_indented(&format!("Already on the latest version ({ver})"));
                } else {
                    spinner.finish_ok_indented(&format!("{current_ver} -> {ver}"));
                }
            }
            Err(e) => {
                has_error = true;
                let clean_err = if let Some(pos) = e.find(" (owner:") {
                    &e[..pos]
                } else {
                    e.as_str()
                };
                spinner.finish_err_indented(clean_err);
            }
        }

        let _ = ssh.disconnect().await;
    }

    if has_error {
        std::process::exit(1);
    }
    Ok(())
}

async fn run_server_upgrade(
    name: &str,
    ssh: &mut SshClient,
    running_version: Option<&str>,
    target: &crate::config::ServerTarget,
) -> Result<Option<String>, String> {
    let owner = build_upgrade_owner(name);
    let mut upgrade_mode_entered = false;
    let mut binary_replaced = false;

    let result: Result<Option<String>, String> = async {
        let status = ssh
            .tako_status()
            .await
            .map_err(|e| format!("Failed to query status: {e}"))?;
        if status != "active" {
            return Err(format!("tako-server not active (status: {status})"));
        }

        let verified_release = resolve_verified_server_release_asset(target)
            .await
            .map_err(|e| format!("Failed to verify release metadata: {e}"))?;

        tracing::debug!("Downloading latest tako-server binary…");
        let _t = output::timed("Binary download");
        let install_output = ssh
            .exec(&remote_binary_replace_command(
                &verified_release.download_url,
                &verified_release.expected_sha256,
            ))
            .await
            .map_err(|e| format!("Binary download failed: {e}"))?;
        drop(_t);
        if !install_output.success() {
            tracing::debug!("Binary replace failed: {}", install_output.stderr.trim());
            let combined = install_output.combined();
            let message =
                first_non_empty_line(combined.trim()).unwrap_or("binary download/install failed");
            return Err(message.to_string());
        }

        let version_after_install = ssh.tako_version().await.ok().flatten();
        if version_after_install.as_deref() == running_version {
            tracing::debug!("Binary unchanged, skipping reload");
            return Ok(version_after_install);
        }
        binary_replaced = true;

        let _t = output::timed("Enter upgrade mode");
        ssh.tako_enter_upgrading(&owner)
            .await
            .map_err(|e| match &e {
                crate::ssh::SshError::CommandFailed(m) => m.clone(),
                other => other.to_string(),
            })?;
        drop(_t);
        upgrade_mode_entered = true;

        let old_pid = ssh
            .tako_server_info()
            .await
            .map_err(|e| format!("Failed to read runtime config: {e}"))?
            .pid;

        tracing::debug!("Reloading server (pid: {old_pid})…");
        let _t = output::timed("Reload + wait for new process");
        ssh.tako_reload()
            .await
            .map_err(|e| format!("Reload failed: {e}"))?;

        let info = wait_for_primary_ready(ssh, UPGRADE_SOCKET_WAIT_TIMEOUT, old_pid, name).await?;
        drop(_t);
        tracing::debug!("New server process ready (pid: {})", info.pid);

        match ssh.tako_exit_upgrading(&owner).await {
            Ok(()) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("does not hold the upgrade lock") {
                    tracing::debug!("Upgrade lock already cleared by new server process");
                } else {
                    return Err(format!("Failed to exit upgrading mode: {e}"));
                }
            }
        }
        upgrade_mode_entered = false;

        let version = ssh.tako_version().await.ok().flatten();
        tracing::debug!("Upgraded (version: {version:?})");

        if let Err(e) = ssh.exec(&remote_cleanup_previous_binary_command()).await {
            tracing::warn!("Failed to remove previous tako-server binary: {e}");
        }
        Ok(version)
    }
    .await;

    if result.is_err() && upgrade_mode_entered {
        tracing::debug!("Upgrade failed, attempting to release upgrade lock (owner: {owner})");
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            match ssh.tako_exit_upgrading(&owner).await {
                Ok(()) => {
                    tracing::debug!("Upgrade lock released (attempt {attempt})");
                    break;
                }
                Err(e) => {
                    tracing::debug!(
                        "Failed to release upgrade lock, retrying (attempt {attempt}): {e}"
                    );
                }
            }
        }
    }

    if result.is_err() && binary_replaced {
        match ssh.exec(&remote_restore_previous_binary_command()).await {
            Ok(output) if output.success() => {
                tracing::warn!("Restored previous tako-server binary after failed upgrade");
            }
            Ok(output) => {
                tracing::warn!(
                    "Failed to restore previous tako-server binary: {}",
                    output.combined().trim()
                );
            }
            Err(e) => {
                tracing::warn!("Failed to restore previous tako-server binary: {e}");
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    const TEST_SERVER_CHECKSUM_MANIFEST: &str = "1111111111111111111111111111111111111111111111111111111111111111  tako-server-linux-x86_64-glibc.tar.zst\n\
         2222222222222222222222222222222222222222222222222222222222222222  tako-server-linux-aarch64-musl.tar.zst\n";
    const TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64: &str = "nZdPJ9zO2xgD3KYpdDWovNaMNko8XtBjcqSJVdNZs0aIwKKfc4pG8g0paADEUHIjwabW80jfj35n5qmEH1ko111qsUUsNwdB0ewUAckN5fvO+tprTmhWsFV9653I7q36LzFT3E3ORNI5JUHLQKqgn15DoOloPR7pi1sU/r4y2FFXJcfBIir0LR5jrR9eXuyPAqDDJSX2QJX19WtEnWNXZsAZUaTsHUtXrlHdqtQDb9fA+pr3w+dVUjg12mYRBi1CJbnxTbrZUyy7+LMDQwXWagTjivHXCaSiZVGz4JGuEMds838wNsy8nfwCqXhffrMXuIb3sOZ6sfPVLZgeUnr12ZpkDjYEiDAz0HEekNQUIIQqjvlcIkgxZYByZLRap0Vvi4NMfPkRI7K7FDtY1hhs7CurJ7Xcag784cx5V+pFEPIbCfMnEjK/beP+V36UbSbjnbOtbw4WUKQZH+knspw+MUBmy3ZdqGsgYDSyVQ6dE5u7lvl4V9/ai8f5pue5uWgL";

    #[test]
    fn build_upgrade_owner_is_shell_safe() {
        let owner = build_upgrade_owner("prod-1");
        assert!(owner.contains("upgrade-prod-1-"));
        assert!(owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn server_binary_download_url_uses_latest_tag() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let url = server_binary_download_url(&target, None, false).unwrap();
        assert_eq!(
            url,
            "https://github.com/lilienblum/tako/releases/download/latest/tako-server-linux-x86_64-glibc.tar.zst"
        );
    }

    #[test]
    fn server_binary_download_url_rejects_insecure_custom_base_without_override() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let err = server_binary_download_url(&target, Some("http://example.test/releases"), false)
            .unwrap_err();
        assert!(err.contains("must use https://"));
    }

    #[test]
    fn server_binary_download_url_allows_insecure_custom_base_with_explicit_override() {
        let target = crate::config::ServerTarget {
            arch: "x86_64".to_string(),
            libc: "glibc".to_string(),
        };
        let url = server_binary_download_url(&target, Some("http://example.test/releases"), true)
            .unwrap();
        assert_eq!(
            url,
            "http://example.test/releases/tako-server-linux-x86_64-glibc.tar.zst"
        );
    }

    #[test]
    fn parse_sha256_manifest_value_finds_named_asset() {
        let sha = parse_sha256_manifest_value(
            TEST_SERVER_CHECKSUM_MANIFEST,
            "tako-server-linux-aarch64-musl.tar.zst",
        )
        .unwrap();
        assert_eq!(
            sha,
            "2222222222222222222222222222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn verify_signed_server_checksum_manifest_accepts_valid_signature() {
        let signature = base64::engine::general_purpose::STANDARD
            .decode(TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64)
            .unwrap();
        verify_signed_server_checksum_manifest(
            TEST_SERVER_CHECKSUM_MANIFEST.as_bytes(),
            &signature,
        )
        .unwrap();
    }

    #[test]
    fn verify_signed_server_checksum_manifest_rejects_tampering() {
        let signature = base64::engine::general_purpose::STANDARD
            .decode(TEST_SERVER_CHECKSUM_MANIFEST_SIG_BASE64)
            .unwrap();
        let err = verify_signed_server_checksum_manifest(
            b"1111111111111111111111111111111111111111111111111111111111111111  tako-server-linux-x86_64-glibc.tar.zst\n",
            &signature,
        )
        .unwrap_err();
        assert!(err.contains("signature verification failed"));
    }

    #[test]
    fn remote_binary_replace_command_uses_root_shell_wrapper_and_verifies_sha256() {
        let cmd = remote_binary_replace_command(
            "https://example.com/tako-server.tar.zst",
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        );
        assert!(cmd.contains("then sh -c '"));
        assert!(cmd.contains("sudo sh -c '"));
        assert!(cmd.contains("curl -fsSL"));
        assert!(cmd.contains("sha256 mismatch"));
        assert!(cmd.contains("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"));
        assert!(cmd.contains("install -m 0755"));
        assert!(cmd.contains("/usr/local/bin/tako-server.prev"));
        assert!(cmd.contains("/usr/local/bin/tako-server"));
    }

    #[test]
    fn remote_restore_previous_binary_command_restores_prev_binary() {
        let cmd = remote_restore_previous_binary_command();
        assert!(cmd.contains("sudo sh -c '"));
        assert!(cmd.contains("previous tako-server binary not found"));
        assert!(cmd.contains("/usr/local/bin/tako-server.prev"));
        assert!(cmd.contains("/usr/local/bin/tako-server"));
    }

    #[test]
    fn remote_cleanup_previous_binary_command_removes_prev_binary() {
        let cmd = remote_cleanup_previous_binary_command();
        assert!(cmd.contains("rm -f /usr/local/bin/tako-server.prev"));
    }

    #[test]
    fn build_upgrade_owner_differs_by_server_name() {
        let a = build_upgrade_owner("prod-1");
        let b = build_upgrade_owner("prod-2");
        assert_ne!(a, b, "different servers should produce different owner IDs");
        assert!(a.contains("prod-1"));
        assert!(b.contains("prod-2"));
    }

    #[test]
    fn first_non_empty_line_skips_blanks() {
        assert_eq!(first_non_empty_line("\n\n  hello\nworld"), Some("hello"));
        assert_eq!(first_non_empty_line(""), None);
        assert_eq!(first_non_empty_line("\n\n"), None);
        assert_eq!(first_non_empty_line("first"), Some("first"));
    }
}

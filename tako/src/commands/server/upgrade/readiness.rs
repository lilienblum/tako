use std::time::Duration;

use tako_core::ServerRuntimeInfo;

use super::UPGRADE_POLL_INTERVAL;

pub(crate) async fn wait_for_primary_ready(
    ssh: &mut crate::ssh::SshClient,
    timeout: Duration,
    old_pid: u32,
    server_name: &str,
    allow_incompatible: bool,
) -> Result<ServerRuntimeInfo, String> {
    let start = std::time::Instant::now();
    let mut last_err = String::new();
    let mut last_seen_pid: Option<u32> = None;
    let mut poll_count = 0u32;
    while start.elapsed() < timeout {
        ssh.clear_tako_hello_cache();
        poll_count += 1;
        match ssh
            .tako_server_info_allow_incompatible(allow_incompatible)
            .await
        {
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

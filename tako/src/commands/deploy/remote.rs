use std::future::Future;
use std::path::Path;
use std::time::Instant;

use crate::output;
use crate::shell::shell_single_quote;
use crate::ssh::{SshClient, SshConfig};
use tako_core::{Command, Response};

use super::DeployConfig;
use super::format::{format_deploy_step_failure, format_size};
use super::task_tree::DeployTaskTreeController;

pub(super) fn parse_existing_routes_response(
    response: Response,
) -> Result<Vec<(String, Vec<String>)>, String> {
    match response {
        Response::Ok { data } => Ok(data
            .get("routes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let app = item.get("app")?.as_str()?.to_string();
                        let routes = item
                            .get("routes")
                            .and_then(|r| r.as_array())
                            .map(|r| {
                                r.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        Some((app, routes))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()),
        Response::Error { message } => Err(format!("tako-server error (routes): {}", message)),
    }
}

pub(super) fn extract_server_error_message(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from))
        .map(|message| {
            message
                .strip_prefix("Deploy failed: ")
                .unwrap_or(&message)
                .to_string()
        })
        .unwrap_or_else(|| response.to_string())
}

pub(super) fn deploy_response_has_error(response: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|status| status.as_str())
                .map(|status| status == "error")
        })
        .unwrap_or(false)
}

pub(super) fn cleanup_partial_release_command(release_dir: &str) -> String {
    format!("rm -rf {}", shell_single_quote(release_dir))
}

pub(super) async fn cleanup_partial_release(
    ssh: &SshClient,
    release_dir: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    ssh.exec_checked(&cleanup_partial_release_command(release_dir))
        .await?;
    Ok(())
}

pub(super) async fn remote_directory_exists(
    ssh: &SshClient,
    path: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let quoted = shell_single_quote(path);
    let cmd = format!("if [ -d {quoted} ]; then echo yes; else echo no; fi");
    let output = ssh.exec(&cmd).await?;
    if !output.success() {
        return Err(format!(
            "Failed to check remote directory existence for {}: {}",
            path,
            output.combined().trim()
        )
        .into());
    }
    Ok(output.stdout.trim() == "yes")
}

pub(super) async fn connect_and_prepare_remote_release_dir(
    ssh: &mut SshClient,
    release_dir: &str,
    shared_dir: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    ssh.connect().await?;
    prepare_remote_release_dir(ssh, release_dir, shared_dir).await
}

/// Prepare the remote release directory on an already-connected SSH session.
pub(super) async fn prepare_remote_release_dir(
    ssh: &SshClient,
    release_dir: &str,
    shared_dir: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let release_dir_preexisted = remote_directory_exists(ssh, release_dir).await?;
    if !release_dir_preexisted {
        ssh.exec_checked(&format!(
            "mkdir -p {} {}",
            shell_single_quote(release_dir),
            shell_single_quote(shared_dir)
        ))
        .await?;
    }

    Ok(release_dir_preexisted)
}

pub(super) async fn run_deploy_step<T, E, Fut>(
    loading: &str,
    success: &str,
    use_spinner: bool,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if use_spinner {
        let error_label = format!("{} failed", loading.trim_end_matches('…'));
        output::with_spinner_async_err(loading, success, &error_label, work)
            .await
            .map_err(Into::into)
    } else {
        tracing::debug!("{}", loading);
        work.await.map_err(Into::into)
    }
}

pub(super) async fn run_task_tree_deploy_step<T, E, Fut>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    run_task_tree_deploy_step_with_detail(task_tree, server_name, step, None, work).await
}

async fn run_task_tree_deploy_step_with_detail<T, E, Fut>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    success_detail: Option<String>,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    run_task_tree_deploy_step_with_detail_and_error_cleanup(
        task_tree,
        server_name,
        step,
        success_detail,
        work,
        || async {},
    )
    .await
}

pub(super) async fn run_task_tree_deploy_step_with_detail_and_error_cleanup<
    T,
    E,
    Fut,
    Cleanup,
    CleanupFut,
>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    success_detail: Option<String>,
    work: Fut,
    cleanup_on_error: Cleanup,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
    Cleanup: FnOnce() -> CleanupFut + Send,
    CleanupFut: Future<Output = ()> + Send,
{
    task_tree.mark_deploy_step_running(server_name, step);
    match work.await {
        Ok(value) => {
            let success_label = match step {
                "connecting" => "Preflight",
                "uploading" => "Uploaded",
                "preparing" => "Prepared",
                "starting" => "Started",
                _ => step,
            };
            task_tree.rename_deploy_step(server_name, step, success_label);
            task_tree.succeed_deploy_step(server_name, step, success_detail);
            Ok(value)
        }
        Err(error) => {
            let message = error.to_string();
            cleanup_on_error().await;
            task_tree.fail_deploy_step(server_name, step, message.clone());
            let failed_label = match step {
                "connecting" => "Preflight failed",
                "uploading" => "Upload failed",
                "preparing" => "Prepare failed",
                "starting" => "Start failed",
                _ => step,
            };
            task_tree.rename_deploy_step(server_name, step, failed_label);
            task_tree.fail_deploy_target_without_detail(server_name);
            task_tree.cancel_pending_deploy_children(server_name, "cancelled");
            Err(error.into())
        }
    }
}

pub(super) fn remote_release_archive_path(release_dir: &str) -> String {
    format!("{release_dir}/artifacts.tar.zst")
}

pub(super) fn build_remote_extract_archive_command(
    release_dir: &str,
    remote_archive: &str,
) -> String {
    format!(
        "tako-server --extract-zstd-archive {} --extract-dest {} && rm -f {}",
        shell_single_quote(remote_archive),
        shell_single_quote(release_dir),
        shell_single_quote(remote_archive)
    )
}

/// Deploy to a single server.
///
/// If `preconnected_ssh` is provided (from the preflight phase), the existing
/// connection is reused and the "Preflight" task-tree step is skipped (it was
/// already marked complete during preflight).  Otherwise a fresh SSH connection
/// is established here.
#[allow(clippy::too_many_arguments)]
pub(super) async fn deploy_to_server(
    config: &DeployConfig,
    server_name: &str,
    server: &crate::config::ServerEntry,
    archive_path: &Path,
    target_label: &str,
    use_spinner: bool,
    task_tree: Option<DeployTaskTreeController>,
    preconnected_ssh: Option<SshClient>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _server_deploy_timer =
        output::timed(&format!("Server deploy ({target_label}:{})", server.port));
    let release_dir = config.release_dir();

    let (mut ssh, release_dir_preexisted) = if let Some(ssh) = preconnected_ssh {
        // Reuse connection from preflight — "Preflight" is already done.
        // Just prepare the remote release directory.
        let preexisted = prepare_remote_release_dir(&ssh, &release_dir, &config.shared_dir())
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e })?;
        (ssh, preexisted)
    } else {
        // No pre-connected client — connect now.
        let ssh_config = SshConfig::from_server(&server.host, server.port);
        let mut ssh = SshClient::new(ssh_config);
        let preexisted = if let Some(task_tree) = &task_tree {
            run_task_tree_deploy_step(
                task_tree,
                server_name,
                "connecting",
                connect_and_prepare_remote_release_dir(
                    &mut ssh,
                    &release_dir,
                    &config.shared_dir(),
                ),
            )
            .await?
        } else {
            run_deploy_step(
                "Preflight",
                "Preflight",
                use_spinner,
                connect_and_prepare_remote_release_dir(
                    &mut ssh,
                    &release_dir,
                    &config.shared_dir(),
                ),
            )
            .await?
        };
        (ssh, preexisted)
    };
    let archive_size_bytes = std::fs::metadata(archive_path)?.len();
    tracing::debug!("Archive size: {}", format_size(archive_size_bytes));
    let mut cleaned_partial_release = false;

    let result = async {
        // Upload artifact (skip if release dir already has it from a previous deploy).
        let remote_archive = remote_release_archive_path(&release_dir);
        if release_dir_preexisted {
            tracing::debug!("Release dir already exists, skipping upload");
            if let Some(task_tree) = &task_tree {
                task_tree.skip_deploy_step(server_name, "uploading", "cached");
            }
        } else {
            let upload_timer = output::timed(&format!(
                "Upload artifact ({})",
                format_size(archive_size_bytes)
            ));
            if let Some(task_tree) = &task_tree {
                let total_size = archive_size_bytes;
                let tt = task_tree.clone();
                let sn = server_name.to_string();
                let upload_started_at = Instant::now();
                run_task_tree_deploy_step_with_detail(
                    task_tree,
                    server_name,
                    "uploading",
                    None,
                    async {
                        ssh.upload_with_progress(
                            archive_path,
                            &remote_archive,
                            Some(Box::new(move |done, _total| {
                                let fraction = if total_size > 0 {
                                    done as f64 / total_size as f64
                                } else {
                                    0.0
                                };
                                tt.update_deploy_step_progress(
                                    &sn,
                                    "uploading",
                                    output::format_transfer_compact_detail(
                                        done,
                                        total_size,
                                        upload_started_at.elapsed(),
                                    ),
                                    fraction,
                                );
                            })),
                        )
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
                    },
                )
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Uploading", &e.to_string()).into()
                })?;
            } else {
                let upload_result: Result<(), Box<dyn std::error::Error + Send + Sync>> =
                    if use_spinner {
                        let tp = std::sync::Arc::new(output::TransferProgress::new(
                            "Uploading",
                            "Uploaded",
                            archive_size_bytes,
                        ));
                        let tp2 = tp.clone();
                        ssh.upload_with_progress(
                            archive_path,
                            &remote_archive,
                            Some(Box::new(move |done, _total| tp2.set_position(done))),
                        )
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                        tp.finish();
                        Ok(())
                    } else {
                        ssh.upload(archive_path, &remote_archive).await.map_err(
                            |e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) },
                        )
                    };
                upload_result.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Uploading", &e.to_string()).into()
                })?;
            }
            drop(upload_timer);
        }

        // Extract archive and symlink shared dirs.
        if !release_dir_preexisted {
            if let Some(task_tree) = &task_tree {
                run_task_tree_deploy_step(task_tree, server_name, "preparing", async {
                    let _t = output::timed("Extract and configure release");
                    let extract_cmd =
                        build_remote_extract_archive_command(&release_dir, &remote_archive);
                    let shared = shell_single_quote(&config.shared_dir());
                    let rel = shell_single_quote(&release_dir);
                    let shared_link_cmd = format!(
                        "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
                        shared, shared, rel
                    );
                    let combined_cmd = format!("{} && {}", extract_cmd, shared_link_cmd);
                    ssh.exec_checked(&combined_cmd).await?;

                    // Download runtime and install production dependencies on the server.
                    let prepare_cmd = Command::PrepareRelease {
                        app: config.app_name.clone(),
                        path: release_dir.clone(),
                    };
                    let json = serde_json::to_string(&prepare_cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Preparing", &e.to_string()).into()
                })?;
            } else {
                run_deploy_step("Preparing…", "Prepared", use_spinner, async {
                    let _t = output::timed("Extract and configure release");
                    let extract_cmd =
                        build_remote_extract_archive_command(&release_dir, &remote_archive);
                    let shared = shell_single_quote(&config.shared_dir());
                    let rel = shell_single_quote(&release_dir);
                    let shared_link_cmd = format!(
                        "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
                        shared, shared, rel
                    );
                    let combined_cmd = format!("{} && {}", extract_cmd, shared_link_cmd);
                    ssh.exec_checked(&combined_cmd).await?;

                    // Download runtime and install production dependencies on the server.
                    let prepare_cmd = Command::PrepareRelease {
                        app: config.app_name.clone(),
                        path: release_dir.clone(),
                    };
                    let json = serde_json::to_string(&prepare_cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Preparing", &e.to_string()).into()
                })?;
            }
        } else if let Some(task_tree) = &task_tree {
            task_tree.skip_deploy_step(server_name, "preparing", "skipped");
        }
        tracing::debug!(
            "{}",
            super::format::format_deploy_main_message(
                &config.main,
                target_label,
                config.use_unified_target_process,
            )
        );

        // Resolve secrets before the starting step to keep it fast.
        let deploy_secrets = match query_remote_secrets_hash(&ssh, &config.app_name).await {
            Some(remote_hash) if remote_hash == config.secrets_hash => None,
            _ => Some(config.env_vars.clone()),
        };

        let start_result = if let Some(task_tree) = &task_tree {
            run_task_tree_deploy_step_with_detail_and_error_cleanup(
                task_tree,
                server_name,
                "starting",
                None,
                async {
                    let cmd = Command::Deploy {
                        app: config.app_name.clone(),
                        version: config.version.clone(),
                        path: release_dir.clone(),
                        routes: config.routes.clone(),
                        secrets: deploy_secrets,
                    };
                    let json = serde_json::to_string(&cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                },
                || {
                    cleaned_partial_release = true;
                    async {
                        if !release_dir_preexisted
                            && let Err(e) = cleanup_partial_release(&ssh, &release_dir).await
                        {
                            tracing::warn!(
                                "Failed to cleanup partial release directory {release_dir}: {e}"
                            );
                        }
                    }
                },
            )
            .await
        } else {
            run_deploy_step("Starting…", "Started", use_spinner, async {
                let cmd = Command::Deploy {
                    app: config.app_name.clone(),
                    version: config.version.clone(),
                    path: release_dir.clone(),
                    routes: config.routes.clone(),
                    secrets: deploy_secrets,
                };
                let json = serde_json::to_string(&cmd)
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                let response = ssh
                    .tako_command(&json)
                    .await
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

                if deploy_response_has_error(&response) {
                    return Err(extract_server_error_message(&response).into());
                }

                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })
            .await
        };
        start_result.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            format_deploy_step_failure("Starting", &e.to_string()).into()
        })?;

        // Post-deploy housekeeping (not timed as part of "Starting").
        ssh.symlink(&release_dir, &config.current_link())
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        let releases_dir = format!("{}/releases", config.remote_base);
        let cleanup_cmd = format!(
            "find {} -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec rm -rf {{}} \\;",
            shell_single_quote(&releases_dir)
        );
        if let Err(e) = ssh.exec(&cleanup_cmd).await {
            tracing::warn!("Failed to clean up old releases: {e}");
        }

        if let Some(task_tree) = &task_tree {
            task_tree.succeed_deploy_target(server_name, None);
        }

        Ok(())
    }
    .await;

    if result.is_err()
        && !release_dir_preexisted
        && !cleaned_partial_release
        && let Err(e) = cleanup_partial_release(&ssh, &release_dir).await
    {
        tracing::warn!("Failed to cleanup partial release directory {release_dir}: {e}");
    }

    // Always disconnect (best-effort).
    let _ = ssh.disconnect().await;

    result
}

/// Query the remote server for the SHA-256 hash of an app's current secrets.
/// Returns `None` if the query fails.
pub(super) async fn query_remote_secrets_hash(ssh: &SshClient, app_name: &str) -> Option<String> {
    let cmd = Command::GetSecretsHash {
        app: app_name.to_string(),
    };
    let json = serde_json::to_string(&cmd).ok()?;
    let response_str = ssh.tako_command(&json).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(&response_str).ok()?;
    if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return None;
    }
    value
        .get("data")
        .and_then(|d| d.get("hash"))
        .and_then(|h| h.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::task_tree::{
        ArtifactBuildGroup, DeployTaskTreeController, UNIFIED_JS_CACHE_TARGET_LABEL,
        build_deploy_tree, deploy_target_task_id, deploy_task_step_id,
    };
    use super::*;
    use crate::ui;
    use crate::ui::TaskState;
    use std::time::Duration;

    #[test]
    fn extract_server_error_message_strips_leading_deploy_failed_prefix() {
        let response =
            r#"{"status":"error","message":"Deploy failed: Warm instance startup failed"}"#;
        assert_eq!(
            extract_server_error_message(response),
            "Warm instance startup failed"
        );
    }

    #[test]
    fn cleanup_partial_release_command_uses_safe_single_quoted_path() {
        let cmd = cleanup_partial_release_command("/opt/tako/apps/a'b/releases/v1");
        assert!(cmd.contains("rm -rf"));
        assert!(cmd.contains("'\\''"));
        assert!(cmd.contains("/opt/tako/apps/"));
    }

    #[test]
    fn parse_existing_routes_from_ok_response_keeps_empty_routes_and_ignores_malformed_entries() {
        let response = Response::Ok {
            data: serde_json::json!({
                "routes": [
                    {"app": "good-a", "routes": ["a.example.com", "*.a.example.com"]},
                    {"app": "missing-routes"},
                    {"routes": ["missing-app.example.com"]},
                    {"app": "good-b", "routes": ["b.example.com/path/*"]}
                ]
            }),
        };

        let parsed = parse_existing_routes_response(response).expect("should parse");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "good-a");
        assert_eq!(parsed[1].0, "missing-routes");
        assert!(parsed[1].1.is_empty());
        assert_eq!(parsed[2].0, "good-b");
    }

    #[test]
    fn parse_existing_routes_from_error_response_returns_message() {
        let response = Response::Error {
            message: "boom".to_string(),
        };
        let err = parse_existing_routes_response(response).unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn deploy_response_error_detection_only_accepts_structured_status_errors() {
        let json_err = r#"{"status":"error","message":"nope"}"#;
        let json_ok = r#"{"status":"ok","data":{}}"#;
        let old_error_shape = r#"{"error":"old-shape"}"#;
        let plain_text = "all good";

        assert!(deploy_response_has_error(json_err));
        assert!(!deploy_response_has_error(json_ok));
        assert!(!deploy_response_has_error(old_error_shape));
        assert!(!deploy_response_has_error(plain_text));
    }

    #[test]
    fn remote_release_archive_path_uses_artifacts_tar_zst_name() {
        let path = remote_release_archive_path("/opt/tako/apps/my-app/releases/v1");
        assert_eq!(path, "/opt/tako/apps/my-app/releases/v1/artifacts.tar.zst");
    }

    #[test]
    fn build_remote_extract_archive_command_uses_tako_server_and_cleanup() {
        let cmd = build_remote_extract_archive_command(
            "/opt/tako/apps/a'b/releases/v1",
            "/opt/tako/apps/a'b/releases/v1/artifacts.tar.zst",
        );
        assert!(cmd.contains("tako-server --extract-zstd-archive"));
        assert!(cmd.contains("--extract-dest"));
        assert!(cmd.contains("rm -f"));
        assert!(cmd.contains("'\\''"));
    }

    fn sample_shared_build_group() -> ArtifactBuildGroup {
        ArtifactBuildGroup {
            build_target_label: "linux-aarch64-musl".to_string(),
            cache_target_label: UNIFIED_JS_CACHE_TARGET_LABEL.to_string(),
            target_labels: vec!["linux-aarch64-musl".to_string()],
            display_target_label: None,
        }
    }

    #[tokio::test]
    async fn deploy_task_tree_marks_preflight_running_before_complete() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        let worker_controller = controller.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            run_task_tree_deploy_step(&worker_controller, "prod-a", "connecting", async move {
                rx.await.expect("test signal should arrive");
                Ok::<(), String>(())
            })
            .await
            .expect("preflight step should succeed");
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = controller.snapshot();
                let deploy_target = snapshot
                    .deploys
                    .iter()
                    .find(|task| task.id == deploy_target_task_id("prod-a"))
                    .expect("deploy target should exist");
                let preflight = deploy_target
                    .find(&deploy_task_step_id("prod-a", "connecting"))
                    .expect("preflight step should exist");
                if matches!(deploy_target.state, TaskState::Running { .. })
                    && matches!(preflight.state, TaskState::Running { .. })
                {
                    assert_eq!(preflight.label, "Preflight");
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("preflight step should enter running state");

        tx.send(()).expect("worker should still be waiting");
        handle.await.expect("worker should finish cleanly");

        let snapshot = controller.snapshot();
        let deploy_target = snapshot
            .deploys
            .iter()
            .find(|task| task.id == deploy_target_task_id("prod-a"))
            .expect("deploy target should exist");
        let preflight = deploy_target
            .find(&deploy_task_step_id("prod-a", "connecting"))
            .expect("preflight step should exist");
        assert_eq!(preflight.label, "Preflight");
        assert!(matches!(preflight.state, TaskState::Succeeded { .. }));
    }

    #[tokio::test]
    async fn deploy_task_tree_step_failure_attaches_detail_to_child_row() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        let err = run_task_tree_deploy_step(&controller, "prod-a", "starting", async {
            Err::<(), String>("Warm instance startup failed".to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Warm instance startup failed");

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(lines.iter().any(|line| line == "✘ Deploy to prod-a failed"));
        assert!(
            !lines
                .iter()
                .any(|line| line == "  Warm instance startup failed")
        );
        assert!(lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );
    }

    #[tokio::test]
    async fn deploy_task_tree_defers_failed_start_state_until_cleanup_finishes() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        let (cleanup_started_tx, cleanup_started_rx) = tokio::sync::oneshot::channel();
        let (cleanup_finish_tx, cleanup_finish_rx) = tokio::sync::oneshot::channel::<()>();

        let task = tokio::spawn({
            let controller = controller.clone();
            async move {
                run_task_tree_deploy_step_with_detail_and_error_cleanup(
                    &controller,
                    "prod-a",
                    "starting",
                    None,
                    async { Err::<(), String>("Warm instance startup failed".to_string()) },
                    move || async move {
                        let _ = cleanup_started_tx.send(());
                        let _ = cleanup_finish_rx.await;
                    },
                )
                .await
                .unwrap_err()
            }
        });

        cleanup_started_rx.await.unwrap();

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(
            lines
                .iter()
                .any(|line| line.ends_with("Deploying to prod-a…"))
        );
        assert!(lines.iter().any(|line| line.contains("Starting…")));
        assert!(!lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            !lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );

        let _ = cleanup_finish_tx.send(());
        let err = task.await.unwrap();
        assert_eq!(err.to_string(), "Warm instance startup failed");

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(lines.iter().any(|line| line == "✘ Deploy to prod-a failed"));
        assert!(lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );
    }
}

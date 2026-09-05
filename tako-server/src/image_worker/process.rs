use super::framing::{read_worker_frame_async, write_worker_frame_async};
use super::{IMAGE_LOG_SOURCE, IMAGE_WORKER_STDERR_LIMIT};
use std::process::Stdio;
use std::time::Instant;
use tako_images::ImageError;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

pub(super) struct ImageWorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    pub(super) idle_since: Instant,
}

impl ImageWorkerProcess {
    pub(super) async fn spawn(app_name: &str) -> Result<Self, ImageError> {
        let exe = worker_executable_path()?;
        let mut command = Command::new(exe);
        command
            .arg("--image-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        isolate_worker_command(&mut command).map_err(|error| {
            tracing::warn!(%error, "Failed to isolate image worker");
            ImageError::TransformFailed
        })?;
        let mut child = command.spawn().map_err(|error| {
            tracing::warn!(
                app = %app_name,
                source = IMAGE_LOG_SOURCE,
                error = %error,
                "Failed to start image worker process"
            );
            ImageError::TransformFailed
        })?;

        if let Some(stderr) = child.stderr.take() {
            drain_worker_stderr(app_name, stderr);
        }

        let stdin = child.stdin.take().ok_or(ImageError::TransformFailed)?;
        let stdout = child.stdout.take().ok_or(ImageError::TransformFailed)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            idle_since: Instant::now(),
        })
    }

    pub(super) fn is_running(&mut self, app_name: &str) -> bool {
        match self.child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                tracing::warn!(
                    app = %app_name,
                    source = IMAGE_LOG_SOURCE,
                    status = %status,
                    "Image worker process exited"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    app = %app_name,
                    source = IMAGE_LOG_SOURCE,
                    error = %error,
                    "Failed to inspect image worker process"
                );
                false
            }
        }
    }

    pub(super) async fn request(
        &mut self,
        app_name: &str,
        input: &[u8],
    ) -> Result<Vec<u8>, ImageError> {
        write_worker_frame_async(&mut self.stdin, input)
            .await
            .map_err(|error| {
                tracing::warn!(
                    app = %app_name,
                    source = IMAGE_LOG_SOURCE,
                    error = %error,
                    "Failed to write image worker request"
                );
                error
            })?;

        read_worker_frame_async(&mut self.stdout)
            .await
            .map_err(|error| {
                tracing::warn!(
                    app = %app_name,
                    source = IMAGE_LOG_SOURCE,
                    error = %error,
                    "Failed to read image worker response"
                );
                error
            })
    }

    pub(super) async fn stop(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

fn isolate_worker_command(command: &mut Command) -> Result<(), String> {
    let isolation = worker_isolation()?;
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("VIPS_CONCURRENCY", "2")
        .current_dir("/");
    // SAFETY: the hook only applies the shared fork-safe process policy.
    unsafe {
        command.pre_exec(move || tako_spawn::install_process_isolation(&isolation));
    }
    Ok(())
}

fn worker_isolation() -> Result<tako_spawn::ProcessIsolation, String> {
    let mut policy = tako_spawn::ProcessIsolation {
        resource_limits: tako_spawn::ResourceLimits {
            open_files: Some(128),
            processes: Some(64),
            address_space_bytes: if cfg!(target_os = "linux") {
                Some(2 * 1024 * 1024 * 1024)
            } else {
                None
            },
        },
        ..Default::default()
    };
    if cfg!(target_os = "linux") {
        let (uid, gid) = crate::unix::lookup_user_ids("tako-images")
            .map_err(|e| e.to_string())?
            .ok_or("image worker identity is not installed")?;
        if uid == 0 || uid == unsafe { libc::geteuid() } {
            return Err("image worker identity must differ from root and the service".into());
        }
        policy.user = Some(tako_spawn::UserIds {
            uid,
            gid,
            supplementary_gids: Vec::new(),
        });
        policy.parent_death_signal = Some(libc::SIGTERM);
    }
    Ok(policy)
}

fn drain_worker_stderr(app_name: &str, mut stderr: ChildStderr) {
    let app_name = app_name.to_string();
    tokio::spawn(async move {
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let mut stderr_truncated = false;
        loop {
            match stderr.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => {
                    let remaining =
                        (IMAGE_WORKER_STDERR_LIMIT as usize).saturating_sub(buffer.len());
                    if remaining == 0 {
                        stderr_truncated = true;
                        continue;
                    }
                    let retained = read.min(remaining);
                    buffer.extend_from_slice(&chunk[..retained]);
                    stderr_truncated |= retained < read;
                }
                Err(error) => {
                    tracing::warn!(
                        app = %app_name,
                        source = IMAGE_LOG_SOURCE,
                        error = %error,
                        "Failed to read image worker stderr"
                    );
                    return;
                }
            }
        }

        if !buffer.is_empty() {
            let stderr = worker_stderr_snippet(&buffer);
            tracing::warn!(
                app = %app_name,
                source = IMAGE_LOG_SOURCE,
                stderr = %stderr,
                stderr_truncated,
                "Image worker wrote to stderr"
            );
        }
    });
}

fn worker_stderr_snippet(bytes: &[u8]) -> String {
    let snippet = String::from_utf8_lossy(bytes)
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    let snippet = snippet.trim();
    if snippet.is_empty() {
        "<empty>".to_string()
    } else {
        snippet.to_string()
    }
}

#[cfg(target_os = "linux")]
fn worker_executable_path() -> Result<std::path::PathBuf, ImageError> {
    // During upgrades, the installed tako-server path can be replaced while
    // the old process is still serving. `/proc/self/exe` spawns the currently
    // running process image instead of resolving the on-disk install path.
    Ok(std::path::PathBuf::from("/proc/self/exe"))
}

#[cfg(not(target_os = "linux"))]
fn worker_executable_path() -> Result<std::path::PathBuf, ImageError> {
    std::env::current_exe().map_err(|_| ImageError::TransformFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires installed tako-images account and service SETUID/SETGID or root"]
    async fn image_child_exec_has_private_identity_and_bounded_resources() {
        let (uid, _) = crate::unix::lookup_user_ids("tako-images")
            .unwrap()
            .unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .env("CONTROL_PLANE_SECRET", "must-not-leak")
            .args(["-c", "cat /proc/self/status; env; ulimit -n; ulimit -v"]);
        isolate_worker_command(&mut command).unwrap();
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        for field in [
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}"),
            "CapEff:\t0000000000000000".into(),
            "CapPrm:\t0000000000000000".into(),
            "CapAmb:\t0000000000000000".into(),
            "NoNewPrivs:\t1".into(),
        ] {
            assert!(text.contains(&field), "missing {field}: {text}");
        }
        assert_eq!(
            text.lines()
                .find_map(|line| line.strip_prefix("Groups:"))
                .map(str::trim),
            Some("")
        );
        assert!(!text.contains("CONTROL_PLANE_SECRET"), "{text}");
        assert!(text.ends_with("128\n2097152\n"), "{text}");
    }

    #[test]
    fn worker_stderr_snippet_is_single_line() {
        assert_eq!(worker_stderr_snippet(b""), "<empty>");
        assert_eq!(
            worker_stderr_snippet(b"first line\nsecond line\r\n"),
            "first line\\nsecond line\\r\\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn worker_executable_path_uses_running_process_image_on_linux() {
        assert_eq!(
            worker_executable_path().expect("worker executable path"),
            std::path::PathBuf::from("/proc/self/exe")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn worker_executable_path_uses_current_exe_off_linux() {
        assert_eq!(
            worker_executable_path().expect("worker executable path"),
            std::env::current_exe().expect("current exe")
        );
    }
}

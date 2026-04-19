use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::control::State;
use crate::protocol::{DevEvent, Response};
use crate::route_pattern::{route_host_matches_request, split_route_pattern};
use crate::state;
use tokio::io::AsyncBufReadExt;

pub(crate) async fn monitor_handoff_pid(
    state: Arc<Mutex<State>>,
    config_path: String,
    project_dir: String,
    pid: u32,
) {
    let sysinfo_pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sysinfo_pid]), false);
        if sys.process(sysinfo_pid).is_none() {
            let mut s = state.lock().unwrap();
            let still_current = s
                .apps
                .get(&config_path)
                .and_then(|a| a.pid)
                .map(|p| p == pid)
                .unwrap_or(false);
            if still_current {
                if let Some(app) = s.apps.get_mut(&config_path) {
                    app.is_idle = true;
                    app.pid = None;
                }
                let route_id = format!("reg:{}", config_path);
                s.routes.set_active(&route_id, false);
                state::remove_pid_file(&project_dir, &config_path);
            }
            tracing::info!(config_path = %config_path, project_dir = %project_dir, pid = pid, still_current = still_current, "handoff'd process exited");
            break;
        }
    }
}

pub(crate) fn broadcast_app_status(state: &Arc<Mutex<State>>, config_path: &str, status: &str) {
    let s = state.lock().unwrap();
    let app_name = s
        .apps
        .get(config_path)
        .map(|a| a.name.clone())
        .unwrap_or_default();
    s.events.broadcast(Response::Event {
        event: DevEvent::AppStatusChanged {
            config_path: config_path.to_string(),
            app_name,
            status: status.to_string(),
        },
    });
}

pub(crate) fn kill_app_process(pid: u32) {
    if pid == 0 {
        return;
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}

pub(crate) fn broadcast_dev_event(state: &Arc<Mutex<State>>, event: DevEvent) {
    let s = state.lock().unwrap();
    s.events.broadcast(Response::Event { event });
}

pub(crate) fn app_name_for(state: &Arc<Mutex<State>>, config_path: &str) -> String {
    let s = state.lock().unwrap();
    s.apps
        .get(config_path)
        .map(|a| a.name.clone())
        .unwrap_or_default()
}

pub(crate) fn push_user_action(buf: &state::LogBuffer, kind: &str) {
    let payload = serde_json::json!({
        "ts": now_unix_millis(),
        "level": "info",
        "scope": "tako",
        "kind": kind,
    });
    buf.push(payload.to_string());
}

pub(crate) fn push_scoped_log(buf: &state::LogBuffer, level: &str, scope: &str, message: &str) {
    let payload = serde_json::json!({
        "ts": now_unix_millis(),
        "level": level.to_ascii_lowercase(),
        "scope": scope,
        "msg": message,
    });
    buf.push(payload.to_string());
}

fn now_unix_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) async fn spawn_and_monitor_app(
    state: Arc<Mutex<State>>,
    config_path: &str,
) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
    let (project_dir, app_clone, log_buffer) = {
        let s = state.lock().unwrap();
        let app = s.apps.get(config_path).ok_or("app not found")?;
        (app.project_dir.clone(), app.clone(), app.log_buffer.clone())
    };
    let app_name = app_clone.name.clone();

    broadcast_dev_event(
        &state,
        DevEvent::AppLaunching {
            config_path: config_path.to_string(),
            app_name: app_name.clone(),
        },
    );

    let (mut child, readiness_fd) = spawn_app(&project_dir, &app_clone).await?;
    let pid = child.id().ok_or("failed to get child PID")?;

    // Store the PID and clear the idle flag immediately so management commands
    // (restart, unregister) work right away without waiting for readiness.
    {
        let mut s = state.lock().unwrap();
        if let Some(app) = s.apps.get_mut(config_path) {
            app.pid = Some(pid);
            app.is_idle = false;
        }
    }

    state::write_pid_file(&project_dir, config_path, pid);
    broadcast_dev_event(
        &state,
        DevEvent::AppPid {
            config_path: config_path.to_string(),
            app_name: app_name.clone(),
            pid,
        },
    );

    // Wait for the app to signal its bound port on fd 4, then activate the route.
    // While inactive, wait_for_active() holds proxy requests for up to 30 s.
    let route_id = format!("reg:{}", config_path);
    activate_after_readiness(&state, config_path, &route_id, readiness_fd).await;

    let state_for_monitor = state.clone();
    let config_for_monitor = config_path.to_string();
    let dir_for_monitor = project_dir.clone();
    let buf_for_monitor = log_buffer.clone();
    tokio::spawn(async move {
        let exit_status = child.wait().await;
        let code_str = exit_status
            .as_ref()
            .ok()
            .and_then(|s| s.code())
            .map(|c| format!("exit code {c}"))
            .unwrap_or_else(|| {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = exit_status.as_ref().ok().and_then(|s| s.signal()) {
                        return format!("killed by signal {sig}");
                    }
                }
                "signal".to_string()
            });

        let (still_current, exit_app_name) = {
            let mut s = state_for_monitor.lock().unwrap();
            let current = s
                .apps
                .get(&config_for_monitor)
                .and_then(|a| a.pid)
                .map(|p| p == pid)
                .unwrap_or(false);

            let app_name = s
                .apps
                .get(&config_for_monitor)
                .map(|a| a.name.clone())
                .unwrap_or_default();

            if current {
                if let Some(app) = s.apps.get_mut(&config_for_monitor) {
                    app.is_idle = true;
                    app.pid = None;
                }
                let route_id = format!("reg:{}", config_for_monitor);
                s.routes.set_active(&route_id, false);
                state::remove_pid_file(&dir_for_monitor, &config_for_monitor);
                s.events.broadcast(Response::Event {
                    event: DevEvent::AppStatusChanged {
                        config_path: config_for_monitor.clone(),
                        app_name: app_name.clone(),
                        status: "idle".to_string(),
                    },
                });
            }
            (current, app_name)
        };

        if still_current {
            let msg = format!("App exited ({code_str})");
            push_scoped_log(&buf_for_monitor, "Fatal", "tako", &msg);
            broadcast_dev_event(
                &state_for_monitor,
                DevEvent::AppProcessExited {
                    config_path: config_for_monitor.clone(),
                    app_name: exit_app_name,
                    message: msg,
                },
            );

            tracing::info!(config_path = %config_for_monitor, pid = pid, "app process exited, marking idle");
        }
    });

    broadcast_dev_event(
        &state,
        DevEvent::AppStarted {
            config_path: config_path.to_string(),
            app_name,
        },
    );

    Ok(pid)
}

/// Create a Unix pipe for readiness signaling.
///
/// The write end is passed to the child as fd 4; the read end is kept by the parent
/// to receive the bound port written by the SDK.
#[cfg(unix)]
fn create_readiness_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: pipe() is a standard POSIX call; fds is a valid 2-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe() returned 0, so both fds are valid.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Read the bound port from the app's readiness pipe (fd 4).
///
/// Returns the port when the SDK writes `{port}\n`, or `None` if the pipe
/// closes without a valid port (e.g. the process crashed before signaling).
#[cfg(unix)]
async fn wait_for_readiness(readiness_fd: OwnedFd) -> Option<u16> {
    let file = tokio::fs::File::from_std(std::fs::File::from(readiness_fd));
    let mut lines = tokio::io::BufReader::new(file).lines();
    match lines.next_line().await {
        Ok(Some(line)) => line.trim().parse::<u16>().ok(),
        _ => None,
    }
}

/// Wait for the app to signal its bound port on fd 4, then activate the proxy route.
///
/// On non-Unix platforms (no readiness pipe), the route is activated immediately.
async fn activate_after_readiness(
    state: &Arc<Mutex<State>>,
    config_path: &str,
    route_id: &str,
    #[cfg(unix)] readiness_fd: Option<OwnedFd>,
    #[cfg(not(unix))] _readiness_fd: Option<()>,
) {
    #[cfg(unix)]
    if let Some(fd) = readiness_fd {
        let port = tokio::time::timeout(std::time::Duration::from_secs(30), wait_for_readiness(fd))
            .await
            .ok()
            .flatten();

        match port {
            Some(port) => {
                let mut s = state.lock().unwrap();
                if let Some(app) = s.apps.get_mut(config_path) {
                    app.upstream_port = port;
                }
                s.routes.activate_with_port(route_id, port);
                return;
            }
            None => {
                tracing::warn!(
                    config_path,
                    "app did not signal a port on fd 4 (crashed or timeout); activating anyway"
                );
            }
        }
    }

    // Fallback: no readiness pipe or port not signaled — activate with existing port.
    state.lock().unwrap().routes.set_active(route_id, true);
}

async fn spawn_app(
    project_dir: &str,
    app: &state::RuntimeApp,
) -> Result<(tokio::process::Child, Option<OwnedFd>), Box<dyn std::error::Error + Send + Sync>> {
    if app.command.is_empty() {
        return Err("app has empty command".into());
    }

    // Create the fd 4 readiness pipe: child writes its bound port, parent reads it.
    #[cfg(unix)]
    let readiness_pipe = create_readiness_pipe().ok();
    #[cfg(unix)]
    let write_raw: Option<std::os::fd::RawFd> = readiness_pipe.as_ref().map(|(_, w)| w.as_raw_fd());

    let mut cmd = tokio::process::Command::new(&app.command[0]);
    if app.command.len() > 1 {
        cmd.args(&app.command[1..]);
    }
    cmd.current_dir(project_dir)
        // PORT=0: the app binds to an OS-assigned port and reports it on fd 4.
        .env("PORT", "0")
        .env("HOST", "127.0.0.1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(move || {
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            // Expose the write end of the readiness pipe as fd 4 in the child.
            if let Some(fd) = write_raw {
                if fd != 4 {
                    if libc::dup2(fd, 4) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(fd);
                }
            } else {
                // No readiness pipe; close fd 4 so the SDK fails silently.
                libc::close(4);
            }
            Ok(())
        });
    }

    let bin_dir = std::path::Path::new(project_dir).join("node_modules/.bin");
    if bin_dir.is_dir() {
        let current_path = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{current_path}", bin_dir.display()));
    }

    for (k, v) in &app.env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;

    // Keep the read end; dropping the write end (OwnedFd) closes it in the parent
    // so the parent gets EOF when the child closes its copy.
    #[cfg(unix)]
    let readiness_fd = readiness_pipe.map(|(read, _write)| read);
    #[cfg(not(unix))]
    let readiness_fd: Option<OwnedFd> = None;

    let log_buffer = app.log_buffer.clone();
    if let Some(stdout) = child.stdout.take() {
        let buf = log_buffer.clone();
        tokio::spawn(async move {
            drain_pipe_to_buffer(stdout, buf, "info").await;
        });
    }
    if let Some(stderr) = child.stderr.take() {
        let buf = log_buffer.clone();
        tokio::spawn(async move {
            drain_pipe_to_buffer(stderr, buf, "warn").await;
        });
    }

    Ok((child, readiness_fd))
}

/// Forward subprocess output to the log buffer.
///
/// Lines that look like JSON objects (start with `{`) are forwarded as-is —
/// the SDK's structured logger emits them and the renderer parses them.
/// Anything else is wrapped as a plain `scope=app` log at `default_level`
/// so raw `console.log` and crash dumps still surface.
async fn drain_pipe_to_buffer(
    pipe: impl tokio::io::AsyncRead + Unpin,
    buf: state::LogBuffer,
    default_level: &str,
) {
    let reader = tokio::io::BufReader::new(pipe);
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim_start().starts_with('{') {
            buf.push(line);
        } else {
            let json = serde_json::json!({
                "ts": now_unix_millis(),
                "level": default_level,
                "scope": "app",
                "msg": line,
            });
            buf.push(json.to_string());
        }
    }
}

pub(crate) async fn handle_wake_on_request(state: Arc<Mutex<State>>, host: String, path: String) {
    let app_info: Option<(String, state::RuntimeApp)> = {
        let mut s = state.lock().unwrap();
        if s.routes.lookup(&host, &path).is_some_and(|(_, _, a, _)| a) {
            return;
        }
        let found = s
            .apps
            .iter()
            .find(|(_, a)| {
                if !a.is_idle {
                    return false;
                }
                a.hosts.iter().any(|route_pattern| {
                    let (pat_host, pat_path) = split_route_pattern(route_pattern);
                    let host_ok = route_host_matches_request(pat_host, &host);
                    if !host_ok {
                        return false;
                    }
                    match pat_path {
                        None => true,
                        Some(_) => true,
                    }
                })
            })
            .map(|(config_path, a)| (config_path.clone(), a.clone()));
        // Atomically claim the spawn: mark is_idle=false while still holding the
        // lock so concurrent wake-on-request tasks see the updated state and bail out.
        if let Some((ref config_path, _)) = found
            && let Some(app) = s.apps.get_mut(config_path)
        {
            app.is_idle = false;
        }
        found
    };

    let Some((config_path, app)) = app_info else {
        return;
    };

    tracing::info!(
        app_name = %app.name,
        host = %host,
        "waking idle app on request"
    );

    match spawn_app(&app.project_dir, &app).await {
        Ok((mut child, readiness_fd)) => {
            let pid = child.id();

            // Store PID immediately so kill/unregister commands work.
            {
                let mut s = state.lock().unwrap();
                if let Some(rt) = s.apps.get_mut(&config_path) {
                    rt.is_idle = false;
                    rt.pid = pid;
                }
            }

            let route_id = format!("reg:{}", config_path);
            activate_after_readiness(&state, &config_path, &route_id, readiness_fd).await;

            if let Some(pid) = pid {
                state::write_pid_file(&app.project_dir, &config_path, pid);
                let state = state.clone();
                let config_path = config_path.clone();
                let project_dir = app.project_dir.clone();
                let log_buffer = app.log_buffer.clone();
                tokio::spawn(async move {
                    let exit_status = child.wait().await;
                    let code_str = exit_status
                        .as_ref()
                        .ok()
                        .and_then(|s| s.code())
                        .map(|c| format!("exit code {c}"))
                        .unwrap_or_else(|| "signal".to_string());

                    let (still_current, exit_app_name) = {
                        let mut s = state.lock().unwrap();
                        let current = s
                            .apps
                            .get(&config_path)
                            .and_then(|a| a.pid)
                            .map(|p| p == pid)
                            .unwrap_or(false);
                        let app_name = s
                            .apps
                            .get(&config_path)
                            .map(|a| a.name.clone())
                            .unwrap_or_default();
                        if current {
                            if let Some(rt) = s.apps.get_mut(&config_path) {
                                rt.is_idle = true;
                                rt.pid = None;
                            }
                            let route_id = format!("reg:{}", config_path);
                            s.routes.set_active(&route_id, false);
                            state::remove_pid_file(&project_dir, &config_path);
                        }
                        (current, app_name)
                    };

                    if still_current {
                        let msg = format!("App exited ({code_str})");
                        push_scoped_log(&log_buffer, "Fatal", "tako", &msg);
                        broadcast_dev_event(
                            &state,
                            DevEvent::AppProcessExited {
                                config_path: config_path.clone(),
                                app_name: exit_app_name,
                                message: msg,
                            },
                        );
                    }
                    tracing::info!(config_path = %config_path, project_dir = %project_dir, pid = pid, still_current = still_current, "wake-spawned process exited");
                });
            }
        }
        Err(e) => {
            tracing::warn!(
                app_name = %app.name,
                error = %e,
                "failed to spawn app for wake-on-request"
            );
        }
    }
}

pub(crate) fn kill_all_app_processes(state: &Arc<Mutex<State>>) {
    let s = state.lock().unwrap();
    for (config_path, app) in &s.apps {
        if let Some(pid) = app.pid
            && pid > 0
        {
            tracing::info!(app = %app.name, pid = pid, "killing app process group on shutdown");
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
            state::remove_pid_file(&app.project_dir, config_path);
        }
    }
}

pub(crate) async fn stale_app_cleanup_loop(state: Arc<Mutex<State>>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    loop {
        ticker.tick().await;
        let mut s = state.lock().unwrap();
        if let Some(db) = &s.db
            && let Ok(removed) = db.cleanup_stale()
        {
            for config_path in &removed {
                s.apps.remove(config_path);
                let route_id = format!("reg:{}", config_path);
                s.routes.remove_app(&route_id);
            }
            if !removed.is_empty() {
                tracing::info!(count = removed.len(), "cleaned up stale app registrations");
            }
        }
    }
}

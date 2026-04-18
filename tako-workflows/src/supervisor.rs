//! Worker process supervisor.
//!
//! One `WorkerSupervisor` per deployed app. Lifecycle:
//!
//! - `workers >= 1` (always-on): spawn N workers on `start`, respawn any that
//!   exit unexpectedly.
//! - `workers == 0` (scale-to-zero): no workers until `wake()` is called
//!   (from enqueue or cron tick). `wake()` spawns one worker if none is
//!   running. When the worker idles out and exits, we don't respawn —
//!   the next `wake()` starts a fresh one.
//!
//! `shutdown(timeout)` SIGTERMs all workers, waits, and SIGKILLs anything
//! still alive after the timeout. Used by the drain path.

use std::collections::HashMap;
use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;

/// Callback invoked once per line of worker stdout/stderr when
/// [`WorkerSpec::log_sink`] is set. `is_stderr` is `true` for stderr.
pub type WorkerLogSink = Arc<dyn Fn(&str, bool) + Send + Sync>;

/// Static configuration for a single app's workers.
#[derive(Clone)]
pub struct WorkerSpec {
    /// Human-readable app identifier (for logs).
    pub app: String,
    /// Number of always-on workers. `0` = scale-to-zero.
    pub workers: u32,
    /// Per-worker concurrency (passed as env var).
    pub concurrency: u32,
    /// Idle-exit timeout for scale-to-zero workers (ms). `0` = never exit.
    pub idle_timeout_ms: u64,
    /// Program + args. E.g. `["bun", "/path/to/tako-worker.mjs"]`.
    pub command: Vec<OsString>,
    /// Working directory for the worker process.
    pub cwd: PathBuf,
    /// Extra env vars (merged on top of `build_base_env`).
    pub env: HashMap<String, String>,
    /// Secrets to hand the worker via fd 3. Mirror of the HTTP
    /// instance's runtime ABI — the SDK reads JSON from fd 3 at startup
    /// and populates `Tako.secrets`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub secrets: HashMap<String, String>,
    /// Optional per-line log sink. When `Some`, the supervisor pipes
    /// stdout/stderr and forwards each line. When `None`, inherits the
    /// parent's stdio (production default — lets journald/systemd capture
    /// it).
    pub log_sink: Option<WorkerLogSink>,
}

impl WorkerSpec {
    /// Env vars this supervisor always sets for workers, independent of
    /// the caller-supplied `env`. Caller's `env` is layered on top.
    fn effective_env(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = self.env.clone();
        env.insert(
            "TAKO_WORKER_CONCURRENCY".into(),
            self.concurrency.to_string(),
        );
        env.insert(
            "TAKO_WORKER_IDLE_TIMEOUT_MS".into(),
            self.idle_timeout_ms.to_string(),
        );
        env
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SupervisorError {
    #[error("worker spec has empty command")]
    EmptyCommand,
    #[error("spawn failed: {0}")]
    Spawn(#[from] std::io::Error),
}

pub struct WorkerSupervisor {
    spec: WorkerSpec,
    state: Arc<Mutex<State>>,
}

struct State {
    children: Vec<Child>,
    shutting_down: bool,
}

impl WorkerSupervisor {
    pub fn new(spec: WorkerSpec) -> Self {
        Self {
            spec,
            state: Arc::new(Mutex::new(State {
                children: Vec::new(),
                shutting_down: false,
            })),
        }
    }

    /// Launch all always-on workers. No-op when `workers == 0`.
    pub async fn start(&self) -> Result<(), SupervisorError> {
        if self.spec.workers == 0 {
            return Ok(());
        }
        let mut state = self.state.lock();
        for _ in 0..self.spec.workers {
            self.spawn_one_locked(&mut state)?;
        }
        Ok(())
    }

    /// Called on enqueue/cron tick. For scale-to-zero (`workers == 0`),
    /// spawns a worker if none is running. For always-on, respawns any
    /// that died. Holds the state lock across the spawn calls so two
    /// concurrent wakes can't both see an empty slot and over-spawn.
    pub fn wake(&self) -> Result<(), SupervisorError> {
        let mut state = self.state.lock();
        if state.shutting_down {
            return Ok(());
        }
        state
            .children
            .retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        let target = if self.spec.workers == 0 {
            if state.children.is_empty() { 1 } else { 0 }
        } else {
            (self.spec.workers as usize).saturating_sub(state.children.len())
        };
        for _ in 0..target {
            self.spawn_one_locked(&mut state)?;
        }
        Ok(())
    }

    /// Returns true while at least one child is running.
    pub fn is_running(&self) -> bool {
        let mut state = self.state.lock();
        state
            .children
            .retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        !state.children.is_empty()
    }

    /// SIGTERM all children, wait for exit, SIGKILL after `drain_timeout`.
    pub async fn shutdown(&self, drain_timeout: Duration) {
        let pids: Vec<u32> = {
            let mut state = self.state.lock();
            state.shutting_down = true;
            state.children.iter_mut().filter_map(|c| c.id()).collect()
        };

        for pid in &pids {
            #[cfg(unix)]
            unsafe {
                libc::kill(*pid as i32, libc::SIGTERM);
            }
            #[cfg(not(unix))]
            let _ = pid;
        }

        let state = self.state.clone();
        let waited = timeout(drain_timeout, async move {
            loop {
                {
                    let mut s = state.lock();
                    s.children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
                    if s.children.is_empty() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        if waited.is_err() {
            // Force-kill stragglers.
            let mut state = self.state.lock();
            for child in state.children.iter_mut() {
                let _ = child.start_kill();
            }
        }
    }

    /// Caller must hold `self.state` so the spawn + push is atomic with
    /// the slot-availability check.
    fn spawn_one_locked(&self, state: &mut State) -> Result<(), SupervisorError> {
        let mut iter = self.spec.command.iter();
        let program = iter.next().ok_or(SupervisorError::EmptyCommand)?;
        let args: Vec<&OsString> = iter.collect();

        let mut cmd = Command::new(program);
        let piped = self.spec.log_sink.is_some();
        cmd.args(args)
            .current_dir(&self.spec.cwd)
            .stdout(if piped {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stderr(if piped {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .stdin(Stdio::null())
            .env_clear();
        // Preserve PATH (needed to find `bun`/`node`/etc.) + inherit HOME.
        for key in ["PATH", "HOME"] {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        for (k, v) in self.spec.effective_env() {
            cmd.env(k, v);
        }

        // Secrets ABI: the SDK reads a JSON object from fd 3 at startup
        // and populates `Tako.secrets`. `secrets_pipe` (read end) must stay
        // alive through `spawn()` so the fork copies a valid fd into the
        // child. The write side runs on a dedicated thread so the parent
        // doesn't deadlock when secrets exceed the OS pipe buffer.
        #[cfg(unix)]
        let secrets_handles = if !self.spec.secrets.is_empty() {
            Some(create_secrets_pipe(&self.spec.secrets)?)
        } else {
            None
        };
        #[cfg(unix)]
        let secrets_fd: Option<RawFd> = secrets_handles
            .as_ref()
            .map(|(read_end, _)| read_end.as_raw_fd());

        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(move || {
                if let Some(fd) = secrets_fd {
                    if fd != 3 {
                        if libc::dup2(fd, 3) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        libc::close(fd);
                    }
                } else {
                    libc::close(3);
                }
                Ok(())
            });
        }

        tracing::info!(
            app = %self.spec.app,
            workers = self.spec.workers,
            "Spawning worker process"
        );

        let spawn_result = cmd.spawn();
        // Parent-owned read end drops here after spawn, keeping the child's
        // fd 3 alive but releasing our end. The writer thread owns the write
        // end; we join it to surface write errors (or reap it on spawn
        // failure once the read end is dropped and the writer sees EPIPE).
        #[cfg(unix)]
        let mut child = match spawn_result {
            Ok(child) => {
                if let Some((read_end, writer)) = secrets_handles {
                    drop(read_end);
                    join_secrets_writer(writer)?;
                }
                child
            }
            Err(error) => {
                // Dropping the read end gives the writer thread EPIPE so it
                // exits instead of wedging on a full pipe buffer. Detaching
                // the JoinHandle is fine — the thread will exit on its own.
                drop(secrets_handles);
                return Err(SupervisorError::Spawn(error));
            }
        };
        #[cfg(not(unix))]
        let mut child = spawn_result?;

        if let Some(sink) = &self.spec.log_sink {
            if let Some(stdout) = child.stdout.take() {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        (sink)(&line, false);
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let sink = sink.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        (sink)(&line, true);
                    }
                });
            }
        }

        state.children.push(child);
        Ok(())
    }
}

/// Create a pipe with secrets JSON streamed from a dedicated writer
/// thread. The thread owns the write end and drops it when done, so the
/// child sees EOF once it has consumed the payload. The write happens
/// off-thread so payloads larger than the OS pipe buffer (16 KiB on
/// macOS, 64 KiB on Linux) don't deadlock the parent — the child hasn't
/// been spawned yet when this returns.
///
/// Caller must keep the returned `OwnedFd` alive through `spawn()` so
/// the fork copies a valid fd into the child, then join the writer
/// handle after spawn to surface any write error.
#[cfg(unix)]
fn create_secrets_pipe(
    secrets: &HashMap<String, String>,
) -> std::io::Result<(OwnedFd, std::thread::JoinHandle<std::io::Result<()>>)> {
    let json = serde_json::to_vec(secrets)
        .map_err(|e| std::io::Error::other(format!("failed to serialize secrets: {e}")))?;

    let mut fds = [0i32; 2];
    // SAFETY: pipe() is a standard POSIX call; fds is a valid 2-element array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe() just returned these file descriptors.
    let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

    let writer_handle = std::thread::spawn(move || -> std::io::Result<()> {
        use std::io::Write;
        let mut writer = std::fs::File::from(write_end);
        writer.write_all(&json)
        // write_end (now `writer`) drops here, closing the fd and giving the
        // child EOF once it has drained the payload.
    });

    Ok((read_end, writer_handle))
}

#[cfg(unix)]
fn join_secrets_writer(
    handle: std::thread::JoinHandle<std::io::Result<()>>,
) -> Result<(), SupervisorError> {
    match handle.join() {
        Ok(result) => result.map_err(SupervisorError::Spawn),
        Err(_) => Err(SupervisorError::Spawn(std::io::Error::other(
            "secrets writer thread panicked",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    fn sleep_spec(cwd: PathBuf, workers: u32, sleep_secs: &str) -> WorkerSpec {
        WorkerSpec {
            app: "test".into(),
            workers,
            concurrency: 1,
            idle_timeout_ms: 0,
            command: vec!["sleep".into(), sleep_secs.into()],
            cwd,
            env: HashMap::new(),
            secrets: HashMap::new(),
            log_sink: None,
        }
    }

    #[tokio::test]
    async fn start_noop_when_workers_zero() {
        let dir = tempdir().unwrap();
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 0, "10"));
        sup.start().await.unwrap();
        assert!(!sup.is_running());
    }

    #[tokio::test]
    async fn start_spawns_n_workers_when_workers_positive() {
        let dir = tempdir().unwrap();
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 2, "10"));
        sup.start().await.unwrap();
        assert!(sup.is_running());
        assert_eq!(sup.state.lock().children.len(), 2);
        sup.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn wake_spawns_one_on_scale_to_zero_when_none_running() {
        let dir = tempdir().unwrap();
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 0, "10"));
        sup.wake().unwrap();
        assert!(sup.is_running());
        sup.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn wake_does_not_oversubscribe_when_already_running() {
        let dir = tempdir().unwrap();
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 0, "10"));
        sup.wake().unwrap();
        sup.wake().unwrap();
        sup.wake().unwrap();
        assert_eq!(sup.state.lock().children.len(), 1);
        sup.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn shutdown_sigterms_children_and_waits() {
        let dir = tempdir().unwrap();
        // Use a short sleep so the child exits promptly on SIGTERM (default
        // disposition for `sleep` is to exit on SIGTERM).
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 1, "60"));
        sup.start().await.unwrap();
        assert!(sup.is_running());
        sup.shutdown(Duration::from_secs(2)).await;
        assert!(!sup.is_running());
    }

    #[tokio::test]
    async fn wake_respawns_missing_always_on_worker() {
        let dir = tempdir().unwrap();
        // Start with 1 always-on worker that sleeps briefly then exits.
        let sup = WorkerSupervisor::new(sleep_spec(dir.path().into(), 1, "0.05"));
        sup.start().await.unwrap();
        // Give it time to exit on its own.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!sup.is_running());
        sup.wake().unwrap();
        assert!(sup.is_running());
        sup.shutdown(Duration::from_secs(1)).await;
    }

    /// Regression: a secrets payload larger than the OS pipe buffer
    /// (16 KiB on macOS, 64 KiB on Linux) used to deadlock the parent
    /// because the write happened synchronously before any child was
    /// spawned to drain it. The writer thread now owns the write end so
    /// `create_secrets_pipe` returns immediately regardless of size.
    #[test]
    #[cfg(unix)]
    fn secrets_pipe_does_not_deadlock_on_large_payload() {
        use std::io::Read;
        use std::os::fd::IntoRawFd;
        use std::sync::mpsc;
        use std::time::Instant;

        let big_value = "x".repeat(128 * 1024);
        let secrets = HashMap::from([("BIG".to_string(), big_value.clone())]);

        let start = Instant::now();
        let (read_end, writer) = create_secrets_pipe(&secrets).expect("create pipe must not block");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "create_secrets_pipe blocked on pipe write"
        );

        let fd = read_end.into_raw_fd();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: fd just came from into_raw_fd and is owned by this thread.
            let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
            let mut buf = String::new();
            let result = file.read_to_string(&mut buf).map(|_| buf);
            let _ = tx.send(result);
        });

        let buf = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reader did not complete — pipe write deadlocked")
            .expect("read pipe");
        writer.join().expect("writer thread").expect("write ok");

        let parsed: serde_json::Value = serde_json::from_str(&buf).expect("valid JSON");
        assert_eq!(parsed["BIG"].as_str(), Some(big_value.as_str()));
    }

    #[tokio::test]
    async fn effective_env_sets_concurrency_and_idle_timeout() {
        let spec = WorkerSpec {
            app: "a".into(),
            workers: 1,
            concurrency: 7,
            idle_timeout_ms: 12_000,
            command: vec!["sleep".into(), "0".into()],
            cwd: ".".into(),
            env: HashMap::from([("FOO".to_string(), "bar".to_string())]),
            secrets: HashMap::new(),
            log_sink: None,
        };
        let env = spec.effective_env();
        assert_eq!(
            env.get("TAKO_WORKER_CONCURRENCY").map(String::as_str),
            Some("7")
        );
        assert_eq!(
            env.get("TAKO_WORKER_IDLE_TIMEOUT_MS").map(String::as_str),
            Some("12000")
        );
        assert_eq!(env.get("FOO").map(String::as_str), Some("bar"));
    }
}

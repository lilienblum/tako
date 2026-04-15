//! Single shared workflow socket.
//!
//! One socket per tako-server instance handles enqueue + worker RPCs for
//! every deployed app. Commands carry an `app` field; the handler uses a
//! lookup closure to find the app's `RunsDb` and supervisor wake function.
//!
//! Path convention: `{data_dir}/workflows.sock` (symlink) →
//! `{data_dir}/workflows-{pid}.sock` (the actual bound socket). Mirrors
//! the management-socket pattern so two tako-server processes can hand
//! over cleanly during upgrade.
//!
//! Auth: filesystem permissions only (`chmod 0600`, owned by the service
//! user). Same trust model as the mgmt socket — any process running as
//! the service user can talk to it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tako_core::{Command, Response};
use tako_socket::serve_jsonl_connection;
use tokio::net::UnixListener;
use tokio::sync::oneshot;

use super::cron::register_schedules;
use super::enqueue::RunsDb;

/// Callback fired whenever an enqueue or signal succeeds for a given app.
/// Used to wake the supervisor (so `workers = 0` scale-to-zero spawns).
pub type OnEnqueue = Arc<dyn Fn() + Send + Sync>;

/// Lookup: given an app name, return the `RunsDb` + `OnEnqueue` wake
/// closure for that app, or `None` if the app isn't registered.
pub type AppLookup = Arc<dyn Fn(&str) -> Option<(Arc<RunsDb>, OnEnqueue)> + Send + Sync>;

/// Handle to the running socket. Drop to stop accepting + remove files.
pub struct EnqueueSocketHandle {
    symlink_path: PathBuf,
    actual_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl EnqueueSocketHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
        let _ = std::fs::remove_file(&self.actual_path);
    }
}

impl Drop for EnqueueSocketHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = std::fs::remove_file(&self.actual_path);
    }
}

/// Bind the workflow socket and start the accept loop.
///
/// `symlink_path` is the well-known path SDKs connect to. The actual bind
/// happens on `{symlink_dir}/workflows-{pid}.sock` and the symlink is
/// atomically swapped to point at it — same pattern as the mgmt socket,
/// so two tako-server processes can hand over without dropping clients.
pub fn spawn(
    symlink_path: impl AsRef<Path>,
    lookup: AppLookup,
) -> std::io::Result<EnqueueSocketHandle> {
    let symlink_path = symlink_path.as_ref().to_path_buf();
    let dir = symlink_path
        .parent()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "socket path has no parent",
            )
        })?
        .to_path_buf();
    std::fs::create_dir_all(&dir)?;

    let pid = std::process::id();
    let stem = symlink_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "workflows".to_string());
    let actual_path = dir.join(format!("{stem}-{pid}.sock"));

    // Stale pid-specific file from a previous run with the same pid.
    let _ = std::fs::remove_file(&actual_path);

    let std_listener = std::os::unix::net::UnixListener::bind(&actual_path)?;
    std_listener.set_nonblocking(true)?;
    let _ = std::fs::set_permissions(&actual_path, std::fs::Permissions::from_mode(0o600));

    // Atomically swap symlink: write temp, rename over target.
    let temp_link = symlink_path.with_extension("tmp");
    let _ = std::fs::remove_file(&temp_link);
    std::os::unix::fs::symlink(&actual_path, &temp_link)?;
    std::fs::rename(&temp_link, &symlink_path)?;

    tracing::info!(
        actual = %actual_path.display(),
        symlink = %symlink_path.display(),
        "Workflow socket listening"
    );

    let listener = UnixListener::from_std(std_listener)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run(listener, lookup, shutdown_rx));

    Ok(EnqueueSocketHandle {
        symlink_path,
        actual_path,
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

async fn run(listener: UnixListener, lookup: AppLookup, shutdown_rx: oneshot::Receiver<()>) {
    tokio::pin!(shutdown_rx);
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        let lookup = lookup.clone();
                        tokio::spawn(async move {
                            let _ = serve_jsonl_connection(
                                stream,
                                move |cmd: Command| {
                                    let lookup = lookup.clone();
                                    async move { handle_command(&lookup, cmd) }
                                },
                                |e| Response::error(format!("invalid request: {e}")),
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!(?e, "workflow socket accept error");
                    }
                }
            }
        }
    }
}

/// Extract the app from any workflow command. Returns None for commands
/// that don't carry an app (none currently — every workflow command does).
fn command_app(cmd: &Command) -> Option<&str> {
    match cmd {
        Command::EnqueueRun { app, .. }
        | Command::RegisterSchedules { app, .. }
        | Command::ClaimRun { app, .. }
        | Command::HeartbeatRun { app, .. }
        | Command::SaveStep { app, .. }
        | Command::CompleteRun { app, .. }
        | Command::CancelRun { app, .. }
        | Command::FailRun { app, .. }
        | Command::DeferRun { app, .. }
        | Command::WaitForEvent { app, .. }
        | Command::Signal { app, .. } => Some(app),
        _ => None,
    }
}

fn handle_command(lookup: &AppLookup, cmd: Command) -> Response {
    let Some(app) = command_app(&cmd) else {
        return Response::error(format!(
            "command {:?} not accepted on the workflow socket",
            std::mem::discriminant(&cmd)
        ));
    };
    let Some((db, on_enqueue)) = lookup(app) else {
        return Response::error(format!("unknown app: {app}"));
    };

    match cmd {
        Command::EnqueueRun {
            name,
            payload,
            opts,
            ..
        } => match db.enqueue(&name, &payload, &opts) {
            Ok(r) => {
                (on_enqueue)();
                Response::ok(r)
            }
            Err(e) => Response::error(format!("enqueue failed: {e}")),
        },
        Command::RegisterSchedules { schedules, .. } => match register_schedules(&db, &schedules) {
            Ok(()) => Response::ok(serde_json::json!({ "count": schedules.len() })),
            Err(e) => Response::error(format!("register_schedules failed: {e}")),
        },
        Command::ClaimRun {
            worker_id,
            names,
            lease_ms,
            ..
        } => match db.claim(&worker_id, &names, lease_ms) {
            Ok(Some(run)) => Response::ok(run),
            Ok(None) => Response::ok(serde_json::Value::Null),
            Err(e) => Response::error(format!("claim failed: {e}")),
        },
        Command::HeartbeatRun { id, lease_ms, .. } => match db.heartbeat(&id, lease_ms) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("heartbeat failed: {e}")),
        },
        Command::SaveStep {
            id,
            step_name,
            result,
            ..
        } => match db.save_step(&id, &step_name, &result) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("save_step failed: {e}")),
        },
        Command::CompleteRun { id, .. } => match db.complete(&id) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("complete failed: {e}")),
        },
        Command::CancelRun { id, reason, .. } => match db.cancel(&id, reason.as_deref()) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("cancel failed: {e}")),
        },
        Command::FailRun {
            id,
            error,
            next_run_at_ms,
            finalize,
            ..
        } => match db.fail(&id, &error, next_run_at_ms, finalize) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("fail failed: {e}")),
        },
        Command::DeferRun { id, wake_at_ms, .. } => match db.defer(&id, wake_at_ms) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("defer failed: {e}")),
        },
        Command::WaitForEvent {
            id,
            step_name,
            event_name,
            timeout_at_ms,
            ..
        } => match db.wait_for_event(&id, &step_name, &event_name, timeout_at_ms) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("wait_for_event failed: {e}")),
        },
        Command::Signal {
            event_name,
            payload,
            ..
        } => match db.signal(&event_name, &payload) {
            Ok(woken) => {
                if woken > 0 {
                    (on_enqueue)();
                }
                Response::ok(serde_json::json!({ "woken": woken }))
            }
            Err(e) => Response::error(format!("signal failed: {e}")),
        },
        _ => unreachable!("command_app already filtered"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tako_core::EnqueueOpts;
    use tako_socket::{read_json_line, write_json_line};
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    fn lookup_for(map: std::collections::HashMap<String, Arc<RunsDb>>) -> AppLookup {
        Arc::new(move |app: &str| {
            map.get(app).map(|db| {
                let noop: OnEnqueue = Arc::new(|| {});
                (db.clone(), noop)
            })
        })
    }

    #[tokio::test]
    async fn enqueue_routes_by_app() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("workflows.sock");
        let db_a = Arc::new(RunsDb::open_in_memory().unwrap());
        let db_b = Arc::new(RunsDb::open_in_memory().unwrap());

        let mut map = std::collections::HashMap::new();
        map.insert("a".to_string(), db_a.clone());
        map.insert("b".to_string(), db_b.clone());
        let handle = spawn(&sock, lookup_for(map)).unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);

        let cmd = Command::EnqueueRun {
            app: "a".into(),
            name: "w".into(),
            payload: serde_json::json!({}),
            opts: EnqueueOpts::default(),
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        let resp: Response = read_json_line(&mut r).await.unwrap().unwrap();
        assert!(resp.is_ok());

        // App 'a' should have one pending run; app 'b' should have zero.
        assert_eq!(db_a.pending_count().unwrap(), 1);
        assert_eq!(db_b.pending_count().unwrap(), 0);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_app_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("workflows.sock");
        let handle = spawn(&sock, lookup_for(Default::default())).unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);

        let cmd = Command::EnqueueRun {
            app: "ghost".into(),
            name: "w".into(),
            payload: serde_json::json!({}),
            opts: EnqueueOpts::default(),
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        let resp: Response = read_json_line(&mut r).await.unwrap().unwrap();
        assert!(resp.error_message().unwrap().contains("unknown app"));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn on_enqueue_fires_for_signal_with_waiters_only() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("workflows.sock");
        let db = Arc::new(RunsDb::open_in_memory().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let counter = count.clone();
        let on_enq: OnEnqueue = Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let db_for_lookup = db.clone();
        let lookup: AppLookup =
            Arc::new(move |_app: &str| Some((db_for_lookup.clone(), on_enq.clone())));
        let handle = spawn(&sock, lookup).unwrap();

        // Signal with no waiters → should NOT fire on_enqueue.
        let stream = UnixStream::connect(&sock).await.unwrap();
        let (_r, mut w) = stream.into_split();
        let cmd = Command::Signal {
            app: "a".into(),
            event_name: "noop".into(),
            payload: serde_json::json!({}),
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);

        // Now seed a waiter and signal again — should fire.
        let r = db
            .enqueue("w", &serde_json::json!({}), &EnqueueOpts::default())
            .unwrap();
        let _ = db.claim("w1", &["w".into()], 30_000).unwrap();
        db.wait_for_event(&r.id, "step", "evt", None).unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (_r, mut w) = stream.into_split();
        let cmd = Command::Signal {
            app: "a".into(),
            event_name: "evt".into(),
            payload: serde_json::json!({"x": 1}),
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_removes_pid_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("workflows.sock");
        let handle = spawn(&sock, lookup_for(Default::default())).unwrap();
        assert!(sock.exists() || sock.is_symlink());
        handle.shutdown().await;
    }
}

//! Per-app unix socket that accepts `EnqueueTask` (and, later,
//! `RegisterSchedules`) from the SDK inside the running app.
//!
//! Path convention: `{data_dir}/apps/<app>/enqueue.sock`. The socket is 0600
//! — same user as tako-server, which also spawns the app/worker processes.
//!
//! Protocol: JSONL, one `Command` per line, one `Response` per line. Uses
//! `tako_socket::serve_jsonl_connection` so framing matches the mgmt socket.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tako_core::{Command, Response};
use tako_socket::serve_jsonl_connection;
use tokio::net::UnixListener;
use tokio::sync::oneshot;

use super::cron::register_schedules;
use super::enqueue::RunsDb;

/// Handle to a running enqueue-socket task. Drop to stop the listener and
/// remove the socket file.
pub struct EnqueueSocketHandle {
    path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl EnqueueSocketHandle {
    /// Stop the listener and wait for it to finish. Equivalent to drop but
    /// lets callers await completion deterministically.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for EnqueueSocketHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Callback fired whenever an enqueue command succeeds. Used to wake the
/// worker supervisor (so `workers = 0` scale-to-zero spawns on demand).
pub type OnEnqueue = Arc<dyn Fn() + Send + Sync>;

/// Bind and start an enqueue socket for a single app.
///
/// `on_enqueue` is called after every successful `EnqueueTask`. Pass a
/// no-op closure if no wake side effect is needed.
pub fn spawn(
    path: impl AsRef<Path>,
    db: Arc<RunsDb>,
    on_enqueue: OnEnqueue,
) -> std::io::Result<EnqueueSocketHandle> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Remove any stale socket from a previous run.
    let _ = std::fs::remove_file(&path);

    let listener = UnixListener::bind(&path)?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run(listener, db, on_enqueue, shutdown_rx));

    Ok(EnqueueSocketHandle {
        path,
        shutdown_tx: Some(shutdown_tx),
        join: Some(join),
    })
}

async fn run(
    listener: UnixListener,
    db: Arc<RunsDb>,
    on_enqueue: OnEnqueue,
    shutdown_rx: oneshot::Receiver<()>,
) {
    tokio::pin!(shutdown_rx);
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _addr)) => {
                        let db = db.clone();
                        let on_enqueue = on_enqueue.clone();
                        tokio::spawn(async move {
                            let _ = serve_jsonl_connection(
                                stream,
                                move |cmd: Command| {
                                    let db = db.clone();
                                    let on_enqueue = on_enqueue.clone();
                                    async move { handle_command(&db, cmd, &on_enqueue) }
                                },
                                |e| Response::error(format!("invalid request: {e}")),
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        tracing::warn!(?e, "enqueue socket accept error");
                    }
                }
            }
        }
    }
}

fn handle_command(db: &RunsDb, cmd: Command, on_enqueue: &OnEnqueue) -> Response {
    match cmd {
        Command::EnqueueRun {
            name,
            payload,
            opts,
            ..
        } => match db.enqueue(&name, &payload, &opts) {
            Ok(r) => {
                // Wake the supervisor — scale-to-zero workers spin up here.
                (on_enqueue)();
                Response::ok(r)
            }
            Err(e) => Response::error(format!("enqueue failed: {e}")),
        },
        Command::RegisterSchedules { schedules, .. } => match register_schedules(db, &schedules) {
            Ok(()) => Response::ok(serde_json::json!({ "count": schedules.len() })),
            Err(e) => Response::error(format!("register_schedules failed: {e}")),
        },
        Command::ClaimRun {
            worker_id,
            names,
            lease_ms,
        } => match db.claim(&worker_id, &names, lease_ms) {
            Ok(Some(task)) => Response::ok(task),
            Ok(None) => Response::ok(serde_json::Value::Null),
            Err(e) => Response::error(format!("claim failed: {e}")),
        },
        Command::HeartbeatRun { id, lease_ms } => match db.heartbeat(&id, lease_ms) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("heartbeat failed: {e}")),
        },
        Command::SaveStep {
            id,
            step_name,
            result,
        } => match db.save_step(&id, &step_name, &result) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("save_step failed: {e}")),
        },
        Command::CompleteRun { id } => match db.complete(&id) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("complete failed: {e}")),
        },
        Command::CancelRun { id, reason } => match db.cancel(&id, reason.as_deref()) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("cancel failed: {e}")),
        },
        Command::FailRun {
            id,
            error,
            next_run_at_ms,
            finalize,
        } => match db.fail(&id, &error, next_run_at_ms, finalize) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("fail failed: {e}")),
        },
        Command::DeferRun { id, wake_at_ms } => match db.defer(&id, wake_at_ms) {
            Ok(()) => Response::ok(serde_json::json!({})),
            Err(e) => Response::error(format!("defer failed: {e}")),
        },
        Command::WaitForEvent {
            id,
            step_name,
            event_name,
            timeout_at_ms,
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
        other => Response::error(format!(
            "command {:?} not accepted on the enqueue socket",
            std::mem::discriminant(&other)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tako_core::EnqueueOpts;
    use tako_socket::{read_json_line, write_json_line};
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    async fn with_socket<F, Fut, R>(f: F) -> R
    where
        F: FnOnce(PathBuf, Arc<RunsDb>) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("enqueue.sock");
        let db = Arc::new(RunsDb::open_in_memory().unwrap());
        let noop: OnEnqueue = Arc::new(|| {});
        let handle = spawn(&sock, db.clone(), noop).unwrap();
        let out = f(sock, db).await;
        handle.shutdown().await;
        out
    }

    #[tokio::test]
    async fn enqueue_over_socket_returns_task_id() {
        with_socket(|path, db| async move {
            let stream = UnixStream::connect(&path).await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut r = BufReader::new(r);

            let cmd = Command::EnqueueRun {
                app: "app".into(),
                name: "send-email".into(),
                payload: serde_json::json!({ "to": "a@b.c" }),
                opts: EnqueueOpts::default(),
            };
            write_json_line(&mut w, &cmd).await.unwrap();

            let resp: Response = read_json_line(&mut r).await.unwrap().unwrap();
            match resp {
                Response::Ok { data } => {
                    let id = data.get("id").and_then(|v| v.as_str()).unwrap().to_string();
                    assert!(!id.is_empty());
                    assert_eq!(
                        data.get("deduplicated").and_then(|v| v.as_bool()),
                        Some(false)
                    );
                }
                Response::Error { message } => panic!("expected ok, got error: {message}"),
            }
            assert_eq!(db.pending_count().unwrap(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn enqueue_deduplicates_on_unique_key_over_socket() {
        with_socket(|path, db| async move {
            async fn send(path: &Path, key: &str) -> Response {
                let stream = UnixStream::connect(path).await.unwrap();
                let (r, mut w) = stream.into_split();
                let mut r = BufReader::new(r);
                let cmd = Command::EnqueueRun {
                    app: "app".into(),
                    name: "w".into(),
                    payload: serde_json::json!({}),
                    opts: EnqueueOpts {
                        unique_key: Some(key.into()),
                        ..Default::default()
                    },
                };
                write_json_line(&mut w, &cmd).await.unwrap();
                read_json_line(&mut r).await.unwrap().unwrap()
            }

            let first = send(&path, "cron:5m:0").await;
            let second = send(&path, "cron:5m:0").await;

            let first_id = first
                .data()
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            let second_id = second
                .data()
                .unwrap()
                .get("id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(first_id, second_id);
            assert_eq!(
                second
                    .data()
                    .unwrap()
                    .get("deduplicated")
                    .and_then(|v| v.as_bool()),
                Some(true)
            );
            assert_eq!(db.pending_count().unwrap(), 1);
        })
        .await;
    }

    #[tokio::test]
    async fn rejects_non_enqueue_commands_with_clear_error() {
        with_socket(|path, _db| async move {
            let stream = UnixStream::connect(&path).await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut r = BufReader::new(r);

            let cmd = Command::Status { app: "x".into() };
            write_json_line(&mut w, &cmd).await.unwrap();
            let resp: Response = read_json_line(&mut r).await.unwrap().unwrap();
            assert!(resp.error_message().unwrap().contains("not accepted"));
        })
        .await;
    }

    #[tokio::test]
    async fn connection_handles_multiple_requests_in_sequence() {
        with_socket(|path, db| async move {
            let stream = UnixStream::connect(&path).await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut r = BufReader::new(r);

            for i in 0..3 {
                let cmd = Command::EnqueueRun {
                    app: "app".into(),
                    name: "w".into(),
                    payload: serde_json::json!({ "i": i }),
                    opts: EnqueueOpts::default(),
                };
                write_json_line(&mut w, &cmd).await.unwrap();
                let resp: Response = read_json_line(&mut r).await.unwrap().unwrap();
                assert!(resp.is_ok());
            }
            assert_eq!(db.pending_count().unwrap(), 3);
        })
        .await;
    }

    #[tokio::test]
    async fn shutdown_removes_socket_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("enqueue.sock");
        let db = Arc::new(RunsDb::open_in_memory().unwrap());
        let noop: OnEnqueue = Arc::new(|| {});
        let handle = spawn(&sock, db, noop).unwrap();
        assert!(sock.exists());
        handle.shutdown().await;
        assert!(!sock.exists());
    }

    #[tokio::test]
    async fn on_enqueue_fires_after_successful_enqueue_task() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("enqueue.sock");
        let db = Arc::new(RunsDb::open_in_memory().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        let on_enq: OnEnqueue = Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let handle = spawn(&sock, db, on_enq).unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (_r, mut w) = stream.into_split();
        let cmd = Command::EnqueueRun {
            app: "a".into(),
            name: "w".into(),
            payload: serde_json::json!({}),
            opts: EnqueueOpts::default(),
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn on_enqueue_does_not_fire_for_register_schedules() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("enqueue.sock");
        let db = Arc::new(RunsDb::open_in_memory().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        let on_enq: OnEnqueue = Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let handle = spawn(&sock, db, on_enq).unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let (_r, mut w) = stream.into_split();
        let cmd = Command::RegisterSchedules {
            app: "a".into(),
            schedules: vec![],
        };
        write_json_line(&mut w, &cmd).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);

        handle.shutdown().await;
    }
}

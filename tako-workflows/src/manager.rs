//! Per-server workflow lifecycle manager.
//!
//! Holds one entry per deployed app, each containing the shared `RunsDb`
//! connection, enqueue socket, cron ticker, and worker supervisor. The
//! manager is the single integration surface that `operations.rs` should
//! call from deploy / stop / delete handlers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::cron::{self, CronTickerHandle};
use super::enqueue::{RunsDb, RunsDbError};
use super::enqueue_socket::{EnqueueSocketHandle, OnEnqueue, spawn as spawn_enqueue_socket};
use super::supervisor::{WorkerSpec, WorkerSupervisor};

/// Per-app workflow resources. Dropping the entry shuts down everything in
/// the right order (supervisor → cron → socket).
pub struct AppWorkflow {
    #[allow(dead_code)] // held to keep the DB connection alive
    db: Arc<RunsDb>,
    supervisor: Arc<WorkerSupervisor>,
    enqueue_socket: Option<EnqueueSocketHandle>,
    cron: Option<CronTickerHandle>,
}

impl AppWorkflow {
    pub fn supervisor(&self) -> Arc<WorkerSupervisor> {
        self.supervisor.clone()
    }

    async fn shutdown(mut self, drain_timeout: Duration) {
        self.supervisor.shutdown(drain_timeout).await;
        if let Some(cron) = self.cron.take() {
            cron.shutdown().await;
        }
        if let Some(sock) = self.enqueue_socket.take() {
            sock.shutdown().await;
        }
    }
}

pub struct WorkflowManager {
    data_dir: PathBuf,
    apps: Mutex<HashMap<String, AppWorkflow>>,
}

#[derive(thiserror::Error, Debug)]
pub enum WorkflowManagerError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("db error: {0}")]
    Db(#[from] RunsDbError),
    #[error("supervisor error: {0}")]
    Supervisor(#[from] super::supervisor::SupervisorError),
}

impl WorkflowManager {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            apps: Mutex::new(HashMap::new()),
        }
    }

    pub fn app_dir(&self, app: &str) -> PathBuf {
        self.data_dir.join("apps").join(app)
    }

    pub fn runs_db_path(&self, app: &str) -> PathBuf {
        self.app_dir(app).join("runs.db")
    }

    pub fn enqueue_socket_path(&self, app: &str) -> PathBuf {
        self.app_dir(app).join("enqueue.sock")
    }

    /// Configure (or reconfigure) workflows for an app. Called on deploy.
    ///
    /// If an entry already exists, this returns early — re-deploying doesn't
    /// shut down a live worker. (Rolling updates are out of scope here; the
    /// HTTP rolling updater is the mechanism that swaps processes.)
    pub async fn ensure(
        &self,
        app: &str,
        spec_fn: impl FnOnce(PathBuf, PathBuf) -> WorkerSpec,
    ) -> Result<(), WorkflowManagerError> {
        {
            let apps = self.apps.lock();
            if apps.contains_key(app) {
                return Ok(());
            }
        }

        let db_path = self.runs_db_path(app);
        let socket_path = self.enqueue_socket_path(app);
        let db = Arc::new(RunsDb::open(&db_path)?);

        let spec = spec_fn(db_path, socket_path.clone());
        let supervisor = Arc::new(WorkerSupervisor::new(spec));
        supervisor.start().await?;

        // Single wake closure shared between the enqueue socket and cron tick.
        // Either trigger spawns a scale-to-zero worker if none is running.
        let sup_for_wake = supervisor.clone();
        let on_enqueue: OnEnqueue = Arc::new(move || {
            let _ = sup_for_wake.wake();
        });
        let enqueue_socket = spawn_enqueue_socket(&socket_path, db.clone(), on_enqueue.clone())?;
        let cron_handle = cron::spawn(db.clone(), on_enqueue);

        let entry = AppWorkflow {
            db,
            supervisor,
            enqueue_socket: Some(enqueue_socket),
            cron: Some(cron_handle),
        };

        let mut apps = self.apps.lock();
        apps.insert(app.to_string(), entry);
        Ok(())
    }

    /// Stop the worker but keep DB + socket around (app is paused).
    pub async fn stop(&self, app: &str, drain_timeout: Duration) {
        let entry = self.apps.lock().remove(app);
        if let Some(entry) = entry {
            entry.shutdown(drain_timeout).await;
        }
    }

    /// Stop the worker and remove per-app data directory entirely.
    pub async fn delete(&self, app: &str, drain_timeout: Duration) {
        self.stop(app, drain_timeout).await;
        let dir = self.app_dir(app);
        let _ = std::fs::remove_file(dir.join("runs.db"));
        let _ = std::fs::remove_file(dir.join("runs.db-wal"));
        let _ = std::fs::remove_file(dir.join("runs.db-shm"));
        let _ = std::fs::remove_file(dir.join("enqueue.sock"));
    }

    /// Get the supervisor for an app (used to call `wake()` on enqueue).
    pub fn supervisor_for(&self, app: &str) -> Option<Arc<WorkerSupervisor>> {
        self.apps.lock().get(app).map(|e| e.supervisor())
    }

    /// True when an app is currently registered.
    pub fn has(&self, app: &str) -> bool {
        self.apps.lock().contains_key(app)
    }

    /// Shut down every app. Called on server shutdown.
    pub async fn shutdown_all(&self, drain_timeout: Duration) {
        let apps: Vec<(String, AppWorkflow)> = {
            let mut guard = self.apps.lock();
            guard.drain().collect()
        };
        for (_, entry) in apps {
            entry.shutdown(drain_timeout).await;
        }
    }
}

// Constructors that work with the Supervisor; kept separate so tests can
// swap in a simpler WorkerSpec.
pub fn worker_spec_for_bun(
    app: &str,
    workers: u32,
    concurrency: u32,
    idle_timeout_ms: u64,
    runs_db: &Path,
    enqueue_socket: &Path,
    bun_path: &Path,
    worker_entry: &Path,
    app_cwd: &Path,
) -> WorkerSpec {
    let mut env = std::collections::HashMap::new();
    env.insert("TAKO_RUNS_DB".into(), runs_db.to_string_lossy().to_string());
    env.insert(
        "TAKO_ENQUEUE_SOCKET".into(),
        enqueue_socket.to_string_lossy().to_string(),
    );

    WorkerSpec {
        app: app.to_string(),
        workers,
        concurrency,
        idle_timeout_ms,
        command: vec![bun_path.into(), worker_entry.into()],
        cwd: app_cwd.to_path_buf(),
        env,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn mgr(dir: &Path) -> WorkflowManager {
        WorkflowManager::new(dir)
    }

    fn dummy_spec(cwd: PathBuf, _db: PathBuf, _sock: PathBuf) -> WorkerSpec {
        WorkerSpec {
            app: "t".into(),
            workers: 0,
            concurrency: 1,
            idle_timeout_ms: 0,
            command: vec!["sleep".into(), "10".into()],
            cwd,
            env: StdHashMap::new(),
        }
    }

    #[tokio::test]
    async fn ensure_creates_db_socket_and_supervisor() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr(tmp.path());
        let cwd = tmp.path().to_path_buf();

        m.ensure("a", |db, sock| dummy_spec(cwd.clone(), db, sock))
            .await
            .unwrap();
        assert!(m.has("a"));
        assert!(m.runs_db_path("a").exists());
        assert!(m.enqueue_socket_path("a").exists());

        m.delete("a", Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn ensure_is_noop_on_second_call() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr(tmp.path());
        let cwd = tmp.path().to_path_buf();

        m.ensure("a", |db, sock| dummy_spec(cwd.clone(), db, sock))
            .await
            .unwrap();
        m.ensure("a", |db, sock| dummy_spec(cwd.clone(), db, sock))
            .await
            .unwrap();
        assert!(m.has("a"));
        m.delete("a", Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn stop_shuts_down_but_delete_removes_files() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr(tmp.path());
        let cwd = tmp.path().to_path_buf();

        m.ensure("a", |db, sock| dummy_spec(cwd.clone(), db, sock))
            .await
            .unwrap();
        m.stop("a", Duration::from_secs(1)).await;
        assert!(!m.has("a"));
        assert!(m.runs_db_path("a").exists(), "stop should keep the DB file");

        m.ensure("a", |db, sock| dummy_spec(cwd.clone(), db, sock))
            .await
            .unwrap();
        m.delete("a", Duration::from_secs(1)).await;
        assert!(
            !m.runs_db_path("a").exists(),
            "delete should remove the DB file"
        );
    }

    #[tokio::test]
    async fn shutdown_all_clears_every_app() {
        let tmp = tempfile::tempdir().unwrap();
        let m = mgr(tmp.path());
        let cwd = tmp.path().to_path_buf();

        for name in ["a", "b", "c"] {
            m.ensure(name, |db, sock| dummy_spec(cwd.clone(), db, sock))
                .await
                .unwrap();
        }
        m.shutdown_all(Duration::from_secs(1)).await;
        for name in ["a", "b", "c"] {
            assert!(!m.has(name));
        }
    }
}

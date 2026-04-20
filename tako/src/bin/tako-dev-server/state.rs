use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use rusqlite::OptionalExtension;
use rusqlite::{Connection, params};
use tokio::sync::mpsc;

const PID_FILE_DIR: &str = ".tako/pids";

/// Persistent app registration (survives server restarts).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub config_path: String,
    pub project_dir: String,
    pub name: String,
    pub variant: Option<String>,
    pub is_enabled: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

// ---------------------------------------------------------------------------
// In-memory log ring buffer — replaces the JSONL file-based log store.
// ---------------------------------------------------------------------------

const LOG_BUFFER_CAPACITY: usize = 500;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: u64,
    pub line: String,
}

struct LogBufferInner {
    entries: VecDeque<LogEntry>,
    next_id: u64,
    capacity: usize,
    subscribers: Vec<mpsc::UnboundedSender<LogEntry>>,
}

/// Thread-safe, clonable log ring buffer.
///
/// Stores up to `capacity` entries per app. When the buffer is full, the oldest
/// entry is dropped. Subscribers receive new entries in real time.
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogBufferInner>>,
}

impl std::fmt::Debug for LogBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogBuffer").finish_non_exhaustive()
    }
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogBufferInner {
                entries: VecDeque::with_capacity(LOG_BUFFER_CAPACITY),
                next_id: 0,
                capacity: LOG_BUFFER_CAPACITY,
                subscribers: Vec::new(),
            })),
        }
    }

    /// Push a line into the buffer. Assigns a sequential ID, trims the oldest
    /// entry if over capacity, and broadcasts to all live subscribers.
    pub fn push(&self, line: String) {
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id += 1;
        let entry = LogEntry {
            id,
            line: line.clone(),
        };
        if inner.entries.len() >= inner.capacity {
            inner.entries.pop_front();
        }
        inner.entries.push_back(entry.clone());
        inner
            .subscribers
            .retain(|tx| tx.send(entry.clone()).is_ok());
    }

    /// Subscribe to the log stream. Returns:
    /// - backlog entries after the given `after` ID (or all buffered if None)
    /// - a receiver for new entries
    /// - whether the requested `after` point was truncated (oldest entries dropped)
    pub fn subscribe(
        &self,
        after: Option<u64>,
    ) -> (Vec<LogEntry>, mpsc::UnboundedReceiver<LogEntry>, bool) {
        let mut inner = self.inner.lock().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();

        let oldest_id = inner.entries.front().map(|e| e.id);
        let truncated = match (after, oldest_id) {
            (Some(req), Some(oldest)) => req < oldest,
            (Some(_), None) => false, // buffer empty, nothing truncated
            (None, _) => false,
        };

        let backlog: Vec<LogEntry> = match after {
            Some(after_id) => inner
                .entries
                .iter()
                .filter(|e| e.id > after_id)
                .cloned()
                .collect(),
            None => inner.entries.iter().cloned().collect(),
        };

        inner.subscribers.push(tx);
        (backlog, rx, truncated)
    }

    /// Clear all entries. Preserves the ID counter so cursor-based resumption
    /// still works across clears. Existing subscribers remain connected.
    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.entries.clear();
    }
}

/// Runtime app state (in-memory only, lost on server restart).
#[derive(Debug, Clone)]
pub struct RuntimeApp {
    pub project_dir: String,
    pub name: String,
    pub variant: Option<String>,
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub is_idle: bool,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub log_buffer: LogBuffer,
    pub pid: Option<u32>,
    pub client_pid: Option<u32>,
}

// ---------------------------------------------------------------------------
// PID file management — {project_dir}/.tako/pids/<config-hash>.pid
// ---------------------------------------------------------------------------

fn pid_file_key(config_path: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config_path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn pid_file_path(project_dir: &str, config_path: &str) -> PathBuf {
    Path::new(project_dir)
        .join(PID_FILE_DIR)
        .join(format!("{}.pid", pid_file_key(config_path)))
}

/// Write the app's PID to a config-scoped pid file.
pub fn write_pid_file(project_dir: &str, config_path: &str, pid: u32) {
    let path = pid_file_path(project_dir, config_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, pid.to_string());
}

/// Remove the PID file for an app.
pub fn remove_pid_file(project_dir: &str, config_path: &str) {
    let _ = std::fs::remove_file(pid_file_path(project_dir, config_path));
}

/// Read the PID for an app's config-scoped pid file, if it exists.
pub fn read_pid_file(project_dir: &str, config_path: &str) -> Option<u32> {
    std::fs::read_to_string(pid_file_path(project_dir, config_path))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Kill any orphaned app process from a previous server run and clean up
/// the PID file. Called on startup for each registered project.
pub fn kill_orphaned_process(project_dir: &str, config_path: &str) {
    let Some(pid) = read_pid_file(project_dir, config_path) else {
        return;
    };
    if pid == 0 {
        remove_pid_file(project_dir, config_path);
        return;
    }
    // Check if the process is still alive.
    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    if alive {
        tracing::info!(
            project_dir = %project_dir,
            config_path = %config_path,
            pid = pid,
            "killing orphaned app process from previous run"
        );
        unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    remove_pid_file(project_dir, config_path);
}

// ---------------------------------------------------------------------------
// SQLite store — persists registration across restarts
// ---------------------------------------------------------------------------

pub struct DevStateStore {
    conn: Connection,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn set_pragmas(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;",
    )
    .map_err(|e| format!("set pragmas: {e}"))
}

impl DevStateStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create db parent: {e}"))?;
        }
        let conn = Connection::open(&path).map_err(|e| format!("open db: {e}"))?;
        set_pragmas(&conn)?;
        ensure_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn register(
        &self,
        config_path: &str,
        project_dir: &str,
        name: &str,
        variant: Option<&str>,
    ) -> Result<(), String> {
        let now = unix_now() as i64;
        self.conn
            .execute(
                "INSERT INTO apps (config_path, project_dir, name, variant, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(config_path) DO UPDATE SET
                    project_dir = excluded.project_dir,
                    name = excluded.name,
                    variant = excluded.variant,
                    updated_at = excluded.updated_at;",
                params![config_path, project_dir, name, variant, now],
            )
            .map_err(|e| format!("register: {e}"))?;
        Ok(())
    }

    pub fn unregister(&self, config_path: &str) -> Result<bool, String> {
        let rows = self
            .conn
            .execute(
                "DELETE FROM apps WHERE config_path = ?1;",
                params![config_path],
            )
            .map_err(|e| format!("unregister: {e}"))?;
        Ok(rows > 0)
    }

    #[cfg(test)]
    pub fn get(&self, config_path: &str) -> Result<Option<RegisteredApp>, String> {
        self.conn
            .query_row(
                "SELECT config_path, project_dir, name, variant, is_enabled, created_at, updated_at
                 FROM apps WHERE config_path = ?1;",
                params![config_path],
                row_to_registered_app,
            )
            .optional()
            .map_err(|e| format!("get: {e}"))
    }

    pub fn list(&self) -> Result<Vec<RegisteredApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT config_path, project_dir, name, variant, is_enabled, created_at, updated_at
                 FROM apps ORDER BY name, config_path;",
            )
            .map_err(|e| format!("prepare list: {e}"))?;
        stmt.query_map([], row_to_registered_app)
            .map_err(|e| format!("list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list collect: {e}"))
    }

    #[cfg(test)]
    pub fn set_enabled(&self, config_path: &str, enabled: bool) -> Result<bool, String> {
        let now = unix_now() as i64;
        let rows = self
            .conn
            .execute(
                "UPDATE apps SET is_enabled = ?1, updated_at = ?2 WHERE config_path = ?3;",
                params![enabled, now, config_path],
            )
            .map_err(|e| format!("set_enabled: {e}"))?;
        Ok(rows > 0)
    }

    pub fn cleanup_stale(&self) -> Result<Vec<String>, String> {
        let apps = self.list()?;
        let mut removed = Vec::new();
        for app in apps {
            if !Path::new(&app.config_path).exists() {
                self.unregister(&app.config_path)?;
                removed.push(app.config_path);
            }
        }
        Ok(removed)
    }
}

fn row_to_registered_app(row: &rusqlite::Row) -> rusqlite::Result<RegisteredApp> {
    Ok(RegisteredApp {
        config_path: row.get(0)?,
        project_dir: row.get(1)?,
        name: row.get(2)?,
        variant: row.get(3)?,
        is_enabled: row.get(4)?,
        created_at: row.get::<_, i64>(5)? as u64,
        updated_at: row.get::<_, i64>(6)? as u64,
    })
}

fn ensure_schema(conn: &Connection) -> Result<(), String> {
    let columns = table_columns(conn, "apps")?;
    if columns.is_empty() {
        return create_apps_table(conn);
    }

    // v0: no migrations — drop and recreate if schema doesn't match.
    let expected = [
        "config_path",
        "project_dir",
        "name",
        "variant",
        "is_enabled",
        "created_at",
        "updated_at",
    ];
    if !expected.iter().all(|col| columns.iter().any(|c| c == col)) {
        conn.execute_batch("DROP TABLE apps;")
            .map_err(|e| format!("drop outdated apps table: {e}"))?;
        return create_apps_table(conn);
    }

    Ok(())
}

fn create_apps_table(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS apps (
            config_path TEXT PRIMARY KEY,
            project_dir TEXT NOT NULL,
            name TEXT NOT NULL,
            variant TEXT,
            is_enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0
        );",
    )
    .map_err(|e| format!("create apps schema: {e}"))
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    conn.prepare(&format!("PRAGMA table_info({table});"))
        .map_err(|e| format!("prepare table info: {e}"))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("query table info: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("collect table info: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, DevStateStore) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = DevStateStore::open(tmp.path().join("dev-server.db")).unwrap();
        (tmp, store)
    }

    #[test]
    fn open_creates_db_and_schema() {
        let (_tmp, store) = temp_store();
        assert!(store.list().unwrap().is_empty());

        let conn = Connection::open(store.conn.path().unwrap()).unwrap();
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(apps);")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            vec![
                "config_path".to_string(),
                "project_dir".to_string(),
                "name".to_string(),
                "variant".to_string(),
                "is_enabled".to_string(),
                "created_at".to_string(),
                "updated_at".to_string(),
            ]
        );
    }

    #[test]
    fn register_and_get() {
        let (_tmp, store) = temp_store();
        store
            .register(
                "/home/user/my-app/tako.toml",
                "/home/user/my-app",
                "my-app",
                None,
            )
            .unwrap();

        let app = store.get("/home/user/my-app/tako.toml").unwrap().unwrap();
        assert_eq!(app.config_path, "/home/user/my-app/tako.toml");
        assert_eq!(app.project_dir, "/home/user/my-app");
        assert_eq!(app.name, "my-app");
        assert!(app.variant.is_none());
        assert!(app.is_enabled);
        assert!(app.created_at > 0);
        assert_eq!(app.created_at, app.updated_at);
    }

    #[test]
    fn register_with_variant() {
        let (_tmp, store) = temp_store();
        store
            .register(
                "/home/user/my-app/tako.toml",
                "/home/user/my-app",
                "my-app",
                Some("staging"),
            )
            .unwrap();

        let app = store.get("/home/user/my-app/tako.toml").unwrap().unwrap();
        assert_eq!(app.name, "my-app");
        assert_eq!(app.variant.as_deref(), Some("staging"));
    }

    #[test]
    fn register_upserts_name_and_updates_timestamp() {
        let (_tmp, store) = temp_store();
        store
            .register("/proj/tako.toml", "/proj", "old-name", None)
            .unwrap();
        let first = store.get("/proj/tako.toml").unwrap().unwrap();

        store
            .register("/proj/tako.toml", "/proj", "new-name", None)
            .unwrap();
        let second = store.get("/proj/tako.toml").unwrap().unwrap();

        assert_eq!(second.name, "new-name");
        assert_eq!(second.created_at, first.created_at);
        assert!(second.updated_at >= first.updated_at);
    }

    #[test]
    fn set_enabled_toggle() {
        let (_tmp, store) = temp_store();
        store
            .register("/proj/tako.toml", "/proj", "app", None)
            .unwrap();

        assert!(store.set_enabled("/proj/tako.toml", false).unwrap());
        assert!(!store.get("/proj/tako.toml").unwrap().unwrap().is_enabled);

        assert!(store.set_enabled("/proj/tako.toml", true).unwrap());
        assert!(store.get("/proj/tako.toml").unwrap().unwrap().is_enabled);

        assert!(!store.set_enabled("/nonexistent/tako.toml", false).unwrap());
    }

    #[test]
    fn unregister_app() {
        let (_tmp, store) = temp_store();
        store
            .register("/proj/tako.toml", "/proj", "app", None)
            .unwrap();

        assert!(store.unregister("/proj/tako.toml").unwrap());
        assert!(store.get("/proj/tako.toml").unwrap().is_none());
        assert!(!store.unregister("/proj/tako.toml").unwrap());
    }

    #[test]
    fn cleanup_stale_removes_apps_without_tako_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = DevStateStore::open(tmp.path().join("db")).unwrap();

        let real_proj = tmp.path().join("real-proj");
        std::fs::create_dir_all(&real_proj).unwrap();
        let real_config = real_proj.join("preview.toml");
        std::fs::write(&real_config, "name = \"real\"").unwrap();

        store
            .register(
                real_config.to_str().unwrap(),
                real_proj.to_str().unwrap(),
                "real",
                None,
            )
            .unwrap();
        store
            .register(
                "/nonexistent/proj/preview.toml",
                "/nonexistent/proj",
                "stale",
                None,
            )
            .unwrap();

        let removed = store.cleanup_stale().unwrap();
        assert_eq!(removed, vec!["/nonexistent/proj/preview.toml"]);

        let apps = store.list().unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "real");
    }

    #[test]
    fn pid_file_write_read_remove() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();
        let config_path = "/tmp/example/tako.toml";

        assert!(read_pid_file(project_dir, config_path).is_none());

        write_pid_file(project_dir, config_path, 12345);
        assert_eq!(read_pid_file(project_dir, config_path), Some(12345));

        remove_pid_file(project_dir, config_path);
        assert!(read_pid_file(project_dir, config_path).is_none());
    }

    #[test]
    fn kill_orphaned_process_cleans_up_stale_pid_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();
        let config_path = "/tmp/example/tako.toml";

        // Write a PID file with a definitely-dead PID.
        write_pid_file(project_dir, config_path, 999_999_999);
        kill_orphaned_process(project_dir, config_path);
        assert!(read_pid_file(project_dir, config_path).is_none());
    }

    #[test]
    fn kill_orphaned_process_kills_live_process() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();
        let config_path = "/tmp/example/tako.toml";

        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        write_pid_file(project_dir, config_path, pid);

        kill_orphaned_process(project_dir, config_path);

        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(read_pid_file(project_dir, config_path).is_none());
    }

    #[test]
    fn pid_files_are_scoped_by_config_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();

        write_pid_file(project_dir, "/tmp/example/one.toml", 111);
        write_pid_file(project_dir, "/tmp/example/two.toml", 222);

        assert_eq!(
            read_pid_file(project_dir, "/tmp/example/one.toml"),
            Some(111)
        );
        assert_eq!(
            read_pid_file(project_dir, "/tmp/example/two.toml"),
            Some(222)
        );
    }

    // -----------------------------------------------------------------------
    // LogBuffer tests
    // -----------------------------------------------------------------------

    #[test]
    fn log_buffer_push_and_subscribe_returns_backlog() {
        let buf = LogBuffer::new();
        buf.push("line-0".to_string());
        buf.push("line-1".to_string());
        buf.push("line-2".to_string());

        let (backlog, _rx, truncated) = buf.subscribe(None);
        assert!(!truncated);
        assert_eq!(backlog.len(), 3);
        assert_eq!(backlog[0].id, 0);
        assert_eq!(backlog[0].line, "line-0");
        assert_eq!(backlog[2].id, 2);
        assert_eq!(backlog[2].line, "line-2");
    }

    #[test]
    fn log_buffer_subscribe_after_returns_entries_after_id() {
        let buf = LogBuffer::new();
        for i in 0..5 {
            buf.push(format!("line-{i}"));
        }

        let (backlog, _rx, truncated) = buf.subscribe(Some(2));
        assert!(!truncated);
        assert_eq!(backlog.len(), 2);
        assert_eq!(backlog[0].id, 3);
        assert_eq!(backlog[1].id, 4);
    }

    #[test]
    fn log_buffer_capacity_drops_oldest() {
        let buf = LogBuffer::new();
        // Push more than capacity (500).
        for i in 0..510 {
            buf.push(format!("line-{i}"));
        }

        let (backlog, _rx, _) = buf.subscribe(None);
        assert_eq!(backlog.len(), 500);
        // Oldest should be id=10 (first 10 were dropped).
        assert_eq!(backlog[0].id, 10);
        assert_eq!(backlog[0].line, "line-10");
    }

    #[test]
    fn log_buffer_truncated_flag_when_after_is_before_oldest() {
        let buf = LogBuffer::new();
        for i in 0..510 {
            buf.push(format!("line-{i}"));
        }

        // Request after=5, but oldest is 10 — truncated.
        let (_backlog, _rx, truncated) = buf.subscribe(Some(5));
        assert!(truncated);

        // Request after=10, oldest is 10 — not truncated.
        let (_backlog, _rx, truncated) = buf.subscribe(Some(10));
        assert!(!truncated);
    }

    #[test]
    fn log_buffer_clear_preserves_id_counter() {
        let buf = LogBuffer::new();
        buf.push("before".to_string());
        buf.clear();
        buf.push("after".to_string());

        let (backlog, _rx, _) = buf.subscribe(None);
        assert_eq!(backlog.len(), 1);
        // ID counter is preserved across clear (was 1 after "before", now 1 for "after").
        assert_eq!(backlog[0].id, 1);
        assert_eq!(backlog[0].line, "after");
    }

    #[tokio::test]
    async fn log_buffer_subscriber_receives_live_entries() {
        let buf = LogBuffer::new();
        let (_backlog, mut rx, _) = buf.subscribe(None);

        buf.push("live-1".to_string());
        buf.push("live-2".to_string());

        let entry = rx.recv().await.unwrap();
        assert_eq!(entry.id, 0);
        assert_eq!(entry.line, "live-1");

        let entry = rx.recv().await.unwrap();
        assert_eq!(entry.id, 1);
        assert_eq!(entry.line, "live-2");
    }

    #[tokio::test]
    async fn log_buffer_dead_subscriber_is_cleaned_up() {
        let buf = LogBuffer::new();
        let (_backlog, rx, _) = buf.subscribe(None);
        drop(rx); // Subscriber disconnects.

        // Pushing should not panic; the dead subscriber is cleaned up.
        buf.push("after-drop".to_string());

        // Verify the entry is still in the buffer.
        let (backlog, _rx2, _) = buf.subscribe(None);
        assert_eq!(backlog.len(), 1);
        assert_eq!(backlog[0].line, "after-drop");
    }
}

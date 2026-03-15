use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
#[cfg(test)]
use rusqlite::OptionalExtension;

const PID_FILE: &str = ".tako/dev.pid";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppStatus {
    Running,
    Idle,
    Stopped,
}

impl AppStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppStatus::Running => "running",
            AppStatus::Idle => "idle",
            AppStatus::Stopped => "stopped",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(AppStatus::Running),
            "idle" => Some(AppStatus::Idle),
            "stopped" => Some(AppStatus::Stopped),
            _ => None,
        }
    }
}

/// Persistent app registration (survives server restarts).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct RegisteredApp {
    pub project_dir: String,
    pub name: String,
    pub is_enabled: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Runtime app state (in-memory only, lost on server restart).
#[derive(Debug, Clone)]
pub struct RuntimeApp {
    pub name: String,
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub status: AppStatus,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub log_path: String,
    pub pid: Option<u32>,
    pub client_pid: Option<u32>,
}

// ---------------------------------------------------------------------------
// PID file management — {project_dir}/.tako/dev.pid
// ---------------------------------------------------------------------------

/// Write the app's PID to `{project_dir}/.tako/dev.pid`.
pub fn write_pid_file(project_dir: &str, pid: u32) {
    let path = Path::new(project_dir).join(PID_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, pid.to_string());
}

/// Remove the PID file for an app.
pub fn remove_pid_file(project_dir: &str) {
    let _ = std::fs::remove_file(Path::new(project_dir).join(PID_FILE));
}

/// Read the PID from `{project_dir}/.tako/dev.pid`, if it exists.
pub fn read_pid_file(project_dir: &str) -> Option<u32> {
    std::fs::read_to_string(Path::new(project_dir).join(PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Kill any orphaned app process from a previous server run and clean up
/// the PID file. Called on startup for each registered project.
pub fn kill_orphaned_process(project_dir: &str) {
    let Some(pid) = read_pid_file(project_dir) else {
        return;
    };
    if pid == 0 {
        remove_pid_file(project_dir);
        return;
    }
    // Check if the process is still alive.
    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    if alive {
        tracing::info!(
            project_dir = %project_dir,
            pid = pid,
            "killing orphaned app process from previous run"
        );
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    remove_pid_file(project_dir);
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

    pub fn register(&self, project_dir: &str, name: &str) -> Result<(), String> {
        let now = unix_now() as i64;
        self.conn
            .execute(
                "INSERT INTO apps (project_dir, name, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(project_dir) DO UPDATE SET
                    name = excluded.name,
                    updated_at = excluded.updated_at;",
                params![project_dir, name, now],
            )
            .map_err(|e| format!("register: {e}"))?;
        Ok(())
    }

    pub fn unregister(&self, project_dir: &str) -> Result<bool, String> {
        let rows = self
            .conn
            .execute(
                "DELETE FROM apps WHERE project_dir = ?1;",
                params![project_dir],
            )
            .map_err(|e| format!("unregister: {e}"))?;
        Ok(rows > 0)
    }

    #[cfg(test)]
    pub fn get(&self, project_dir: &str) -> Result<Option<RegisteredApp>, String> {
        self.conn
            .query_row(
                "SELECT project_dir, name, is_enabled, created_at, updated_at
                 FROM apps WHERE project_dir = ?1;",
                params![project_dir],
                row_to_registered_app,
            )
            .optional()
            .map_err(|e| format!("get: {e}"))
    }

    pub fn list(&self) -> Result<Vec<RegisteredApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_dir, name, is_enabled, created_at, updated_at
                 FROM apps ORDER BY name, project_dir;",
            )
            .map_err(|e| format!("prepare list: {e}"))?;
        stmt.query_map([], row_to_registered_app)
            .map_err(|e| format!("list: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list collect: {e}"))
    }

    #[cfg(test)]
    pub fn set_enabled(&self, project_dir: &str, enabled: bool) -> Result<bool, String> {
        let now = unix_now() as i64;
        let rows = self
            .conn
            .execute(
                "UPDATE apps SET is_enabled = ?1, updated_at = ?2 WHERE project_dir = ?3;",
                params![enabled, now, project_dir],
            )
            .map_err(|e| format!("set_enabled: {e}"))?;
        Ok(rows > 0)
    }

    pub fn cleanup_stale(&self) -> Result<Vec<String>, String> {
        let apps = self.list()?;
        let mut removed = Vec::new();
        for app in apps {
            let toml_path = Path::new(&app.project_dir).join("tako.toml");
            if !toml_path.exists() {
                self.unregister(&app.project_dir)?;
                removed.push(app.project_dir);
            }
        }
        Ok(removed)
    }
}

fn row_to_registered_app(row: &rusqlite::Row) -> rusqlite::Result<RegisteredApp> {
    Ok(RegisteredApp {
        project_dir: row.get(0)?,
        name: row.get(1)?,
        is_enabled: row.get(2)?,
        created_at: row.get::<_, i64>(3)? as u64,
        updated_at: row.get::<_, i64>(4)? as u64,
    })
}

fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS apps (
            project_dir TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            is_enabled INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0
        );",
    )
    .map_err(|e| format!("create apps schema: {e}"))
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
                "project_dir".to_string(),
                "name".to_string(),
                "is_enabled".to_string(),
                "created_at".to_string(),
                "updated_at".to_string(),
            ]
        );
    }

    #[test]
    fn register_and_get() {
        let (_tmp, store) = temp_store();
        store.register("/home/user/my-app", "my-app").unwrap();

        let app = store.get("/home/user/my-app").unwrap().unwrap();
        assert_eq!(app.name, "my-app");
        assert!(app.is_enabled);
        assert!(app.created_at > 0);
        assert_eq!(app.created_at, app.updated_at);
    }

    #[test]
    fn register_upserts_name_and_updates_timestamp() {
        let (_tmp, store) = temp_store();
        store.register("/proj", "old-name").unwrap();
        let first = store.get("/proj").unwrap().unwrap();

        store.register("/proj", "new-name").unwrap();
        let second = store.get("/proj").unwrap().unwrap();

        assert_eq!(second.name, "new-name");
        assert_eq!(second.created_at, first.created_at);
        assert!(second.updated_at >= first.updated_at);
    }

    #[test]
    fn set_enabled_toggle() {
        let (_tmp, store) = temp_store();
        store.register("/proj", "app").unwrap();

        assert!(store.set_enabled("/proj", false).unwrap());
        assert!(!store.get("/proj").unwrap().unwrap().is_enabled);

        assert!(store.set_enabled("/proj", true).unwrap());
        assert!(store.get("/proj").unwrap().unwrap().is_enabled);

        assert!(!store.set_enabled("/nonexistent", false).unwrap());
    }

    #[test]
    fn unregister_app() {
        let (_tmp, store) = temp_store();
        store.register("/proj", "app").unwrap();

        assert!(store.unregister("/proj").unwrap());
        assert!(store.get("/proj").unwrap().is_none());
        assert!(!store.unregister("/proj").unwrap());
    }

    #[test]
    fn cleanup_stale_removes_apps_without_tako_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = DevStateStore::open(tmp.path().join("db")).unwrap();

        let real_proj = tmp.path().join("real-proj");
        std::fs::create_dir_all(&real_proj).unwrap();
        std::fs::write(real_proj.join("tako.toml"), "name = \"real\"").unwrap();

        store
            .register(real_proj.to_str().unwrap(), "real")
            .unwrap();
        store.register("/nonexistent/proj", "stale").unwrap();

        let removed = store.cleanup_stale().unwrap();
        assert_eq!(removed, vec!["/nonexistent/proj"]);

        let apps = store.list().unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "real");
    }

    #[test]
    fn pid_file_write_read_remove() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();

        assert!(read_pid_file(project_dir).is_none());

        write_pid_file(project_dir, 12345);
        assert_eq!(read_pid_file(project_dir), Some(12345));

        remove_pid_file(project_dir);
        assert!(read_pid_file(project_dir).is_none());
    }

    #[test]
    fn kill_orphaned_process_cleans_up_stale_pid_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();

        // Write a PID file with a definitely-dead PID.
        write_pid_file(project_dir, 999_999_999);
        kill_orphaned_process(project_dir);
        assert!(read_pid_file(project_dir).is_none());
    }

    #[test]
    fn kill_orphaned_process_kills_live_process() {
        let tmp = tempfile::TempDir::new().unwrap();
        let project_dir = tmp.path().to_str().unwrap();

        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let pid = child.id();
        write_pid_file(project_dir, pid);

        kill_orphaned_process(project_dir);

        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(read_pid_file(project_dir).is_none());
    }
}

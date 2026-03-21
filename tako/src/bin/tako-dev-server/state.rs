use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
use rusqlite::OptionalExtension;
use rusqlite::{Connection, params};

const PID_FILE_DIR: &str = ".tako/dev-pids";

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
    pub config_path: String,
    pub project_dir: String,
    pub name: String,
    pub variant: Option<String>,
    pub is_enabled: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Runtime app state (in-memory only, lost on server restart).
#[derive(Debug, Clone)]
pub struct RuntimeApp {
    pub project_dir: String,
    pub name: String,
    pub variant: Option<String>,
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
// PID file management — {project_dir}/.tako/dev-pids/<config-hash>.pid
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
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
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
}

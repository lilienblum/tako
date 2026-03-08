use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

#[allow(dead_code)]
const WARM_PERIOD_SECS: u64 = 600; // 10 minutes

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

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DevApp {
    pub project_dir: String,
    pub app_name: String,
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub status: AppStatus,
    pub command: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub log_path: String,
    pub pid: Option<u32>,
    pub client_pid: Option<u32>,
    pub updated_at: u64,
}

pub struct DevStateStore {
    db_path: PathBuf,
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
         PRAGMA foreign_keys = ON;
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

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS apps (
                project_dir TEXT PRIMARY KEY,
                app_name TEXT NOT NULL,
                hosts_json TEXT NOT NULL,
                upstream_port INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'stopped',
                command_json TEXT NOT NULL,
                env_json TEXT NOT NULL,
                log_path TEXT NOT NULL,
                pid INTEGER,
                client_pid INTEGER,
                updated_at INTEGER NOT NULL
            );",
        )
        .map_err(|e| format!("migrate: {e}"))?;

        // Add column for databases that predate client_pid tracking.
        let _ = conn.execute_batch("ALTER TABLE apps ADD COLUMN client_pid INTEGER;");

        Ok(Self {
            db_path: path,
            conn,
        })
    }

    /// Return the database file path (for opening independent read-only connections).
    pub fn path(&self) -> PathBuf {
        self.db_path.clone()
    }

    pub fn register_app(
        &self,
        project_dir: &str,
        app_name: &str,
        hosts: &[String],
        upstream_port: u16,
        status: &AppStatus,
        command: &[String],
        env: &std::collections::HashMap<String, String>,
        log_path: &str,
        client_pid: Option<u32>,
    ) -> Result<(), String> {
        let hosts_json =
            serde_json::to_string(hosts).map_err(|e| format!("serialize hosts: {e}"))?;
        let command_json =
            serde_json::to_string(command).map_err(|e| format!("serialize command: {e}"))?;
        let env_json = serde_json::to_string(env).map_err(|e| format!("serialize env: {e}"))?;
        let now = unix_now();

        self.conn.execute(
            "INSERT INTO apps (project_dir, app_name, hosts_json, upstream_port, status, command_json, env_json, log_path, client_pid, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(project_dir) DO UPDATE SET
                app_name = excluded.app_name,
                hosts_json = excluded.hosts_json,
                upstream_port = excluded.upstream_port,
                status = excluded.status,
                command_json = excluded.command_json,
                env_json = excluded.env_json,
                log_path = excluded.log_path,
                client_pid = excluded.client_pid,
                updated_at = excluded.updated_at;",
            params![project_dir, app_name, hosts_json, upstream_port as i64, status.as_str(), command_json, env_json, log_path, client_pid.map(|p| p as i64), now as i64],
        )
        .map_err(|e| format!("register_app: {e}"))?;
        Ok(())
    }

    pub fn set_status(&self, project_dir: &str, status: &AppStatus) -> Result<bool, String> {
        let now = unix_now();
        let rows = self
            .conn
            .execute(
                "UPDATE apps SET status = ?1, updated_at = ?2 WHERE project_dir = ?3;",
                params![status.as_str(), now as i64, project_dir],
            )
            .map_err(|e| format!("set_status: {e}"))?;
        Ok(rows > 0)
    }

    pub fn set_pid(&self, project_dir: &str, pid: Option<u32>) -> Result<bool, String> {
        let now = unix_now();
        let pid_val: Option<i64> = pid.map(|p| p as i64);
        let rows = self
            .conn
            .execute(
                "UPDATE apps SET pid = ?1, updated_at = ?2 WHERE project_dir = ?3;",
                params![pid_val, now as i64, project_dir],
            )
            .map_err(|e| format!("set_pid: {e}"))?;
        Ok(rows > 0)
    }

    pub fn get_app(&self, project_dir: &str) -> Result<Option<DevApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_dir, app_name, hosts_json, upstream_port, status, command_json, env_json, log_path, pid, client_pid, updated_at
                 FROM apps WHERE project_dir = ?1;",
            )
            .map_err(|e| format!("prepare get_app: {e}"))?;

        let app = stmt
            .query_row(params![project_dir], row_to_dev_app)
            .optional()
            .map_err(|e| format!("get_app: {e}"))?;
        Ok(app)
    }

    #[allow(dead_code)]
    pub fn get_app_by_name(&self, app_name: &str) -> Result<Option<DevApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_dir, app_name, hosts_json, upstream_port, status, command_json, env_json, log_path, pid, client_pid, updated_at
                 FROM apps WHERE app_name = ?1 LIMIT 1;",
            )
            .map_err(|e| format!("prepare get_app_by_name: {e}"))?;

        let app = stmt
            .query_row(params![app_name], row_to_dev_app)
            .optional()
            .map_err(|e| format!("get_app_by_name: {e}"))?;
        Ok(app)
    }

    pub fn list_apps(&self) -> Result<Vec<DevApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_dir, app_name, hosts_json, upstream_port, status, command_json, env_json, log_path, pid, client_pid, updated_at
                 FROM apps ORDER BY app_name;",
            )
            .map_err(|e| format!("prepare list_apps: {e}"))?;

        let apps = stmt
            .query_map([], row_to_dev_app)
            .map_err(|e| format!("list_apps: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list_apps collect: {e}"))?;
        Ok(apps)
    }

    pub fn list_active_apps(&self) -> Result<Vec<DevApp>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_dir, app_name, hosts_json, upstream_port, status, command_json, env_json, log_path, pid, client_pid, updated_at
                 FROM apps WHERE status IN ('running', 'idle') ORDER BY app_name;",
            )
            .map_err(|e| format!("prepare list_active_apps: {e}"))?;

        let apps = stmt
            .query_map([], row_to_dev_app)
            .map_err(|e| format!("list_active_apps: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("list_active_apps collect: {e}"))?;
        Ok(apps)
    }

    pub fn delete_app(&self, project_dir: &str) -> Result<bool, String> {
        let rows = self
            .conn
            .execute(
                "DELETE FROM apps WHERE project_dir = ?1;",
                params![project_dir],
            )
            .map_err(|e| format!("delete_app: {e}"))?;
        Ok(rows > 0)
    }

    pub fn cleanup_stale(&self) -> Result<Vec<String>, String> {
        let apps = self.list_apps()?;
        let mut removed = Vec::new();
        for app in apps {
            let toml_path = Path::new(&app.project_dir).join("tako.toml");
            if !toml_path.exists() {
                self.delete_app(&app.project_dir)?;
                removed.push(app.project_dir);
            }
        }
        Ok(removed)
    }

    #[allow(dead_code)]
    pub fn warm_period_secs() -> u64 {
        WARM_PERIOD_SECS
    }

    #[allow(dead_code)]
    pub fn has_any_apps(&self) -> Result<bool, String> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM apps;", [], |row| row.get(0))
            .map_err(|e| format!("has_any_apps: {e}"))?;
        Ok(count > 0)
    }
}

fn row_to_dev_app(row: &rusqlite::Row) -> rusqlite::Result<DevApp> {
    let hosts_json: String = row.get(2)?;
    let command_json: String = row.get(5)?;
    let env_json: String = row.get(6)?;
    let status_str: String = row.get(4)?;
    let pid: Option<i64> = row.get(8)?;
    let client_pid: Option<i64> = row.get(9)?;

    let hosts: Vec<String> = serde_json::from_str(&hosts_json).unwrap_or_default();
    let command: Vec<String> = serde_json::from_str(&command_json).unwrap_or_default();
    let env: std::collections::HashMap<String, String> =
        serde_json::from_str(&env_json).unwrap_or_default();
    let status = AppStatus::from_str(&status_str).unwrap_or(AppStatus::Stopped);

    Ok(DevApp {
        project_dir: row.get(0)?,
        app_name: row.get(1)?,
        hosts,
        upstream_port: row.get::<_, i64>(3)? as u16,
        status,
        command,
        env,
        log_path: row.get(7)?,
        pid: pid.map(|p| p as u32),
        client_pid: client_pid.map(|p| p as u32),
        updated_at: row.get::<_, i64>(10)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_store() -> (tempfile::TempDir, DevStateStore) {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = DevStateStore::open(tmp.path().join("dev-server.db")).unwrap();
        (tmp, store)
    }

    #[test]
    fn open_creates_db_and_schema() {
        let (_tmp, store) = temp_store();
        assert!(store.list_apps().unwrap().is_empty());
    }

    #[test]
    fn register_and_get_app() {
        let (_tmp, store) = temp_store();
        let hosts = vec!["my-app.tako.test".to_string()];
        let cmd = vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()];
        let env = HashMap::from([("NODE_ENV".to_string(), "development".to_string())]);

        store
            .register_app(
                "/home/user/my-app",
                "my-app",
                &hosts,
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/home/user/.tako/dev/logs/my-app.jsonl",
                None,
            )
            .unwrap();

        let app = store.get_app("/home/user/my-app").unwrap().unwrap();
        assert_eq!(app.app_name, "my-app");
        assert_eq!(app.hosts, hosts);
        assert_eq!(app.upstream_port, 3000);
        assert_eq!(app.status, AppStatus::Running);
        assert_eq!(app.command, cmd);
        assert_eq!(app.env, env);
        assert!(app.pid.is_none());
    }

    #[test]
    fn register_upserts_on_conflict() {
        let (_tmp, store) = temp_store();
        let hosts1 = vec!["old.tako.test".to_string()];
        let hosts2 = vec!["new.tako.test".to_string()];
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();

        store
            .register_app(
                "/proj",
                "app1",
                &hosts1,
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        store
            .register_app(
                "/proj",
                "app1",
                &hosts2,
                4000,
                &AppStatus::Idle,
                &cmd,
                &env,
                "/log2",
                None,
            )
            .unwrap();

        let apps = store.list_apps().unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].hosts, hosts2);
        assert_eq!(apps[0].upstream_port, 4000);
        assert_eq!(apps[0].status, AppStatus::Idle);
        assert_eq!(apps[0].log_path, "/log2");
    }

    #[test]
    fn set_status_transitions() {
        let (_tmp, store) = temp_store();
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/proj",
                "app",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();

        assert!(store.set_status("/proj", &AppStatus::Idle).unwrap());
        assert_eq!(
            store.get_app("/proj").unwrap().unwrap().status,
            AppStatus::Idle
        );

        assert!(store.set_status("/proj", &AppStatus::Stopped).unwrap());
        assert_eq!(
            store.get_app("/proj").unwrap().unwrap().status,
            AppStatus::Stopped
        );

        // Non-existent project returns false.
        assert!(
            !store
                .set_status("/nonexistent", &AppStatus::Running)
                .unwrap()
        );
    }

    #[test]
    fn set_and_clear_pid() {
        let (_tmp, store) = temp_store();
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/proj",
                "app",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();

        store.set_pid("/proj", Some(12345)).unwrap();
        assert_eq!(store.get_app("/proj").unwrap().unwrap().pid, Some(12345));

        store.set_pid("/proj", None).unwrap();
        assert!(store.get_app("/proj").unwrap().unwrap().pid.is_none());
    }

    #[test]
    fn get_app_by_name() {
        let (_tmp, store) = temp_store();
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/proj/a",
                "alpha",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        store
            .register_app(
                "/proj/b",
                "beta",
                &[],
                3001,
                &AppStatus::Idle,
                &cmd,
                &env,
                "/log2",
                None,
            )
            .unwrap();

        let app = store.get_app_by_name("beta").unwrap().unwrap();
        assert_eq!(app.project_dir, "/proj/b");

        assert!(store.get_app_by_name("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_active_apps_filters_stopped() {
        let (_tmp, store) = temp_store();
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/a",
                "a",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        store
            .register_app(
                "/b",
                "b",
                &[],
                3001,
                &AppStatus::Idle,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        store
            .register_app(
                "/c",
                "c",
                &[],
                3002,
                &AppStatus::Stopped,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();

        let active = store.list_active_apps().unwrap();
        assert_eq!(active.len(), 2);
        let names: Vec<&str> = active.iter().map(|a| a.app_name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn delete_app() {
        let (_tmp, store) = temp_store();
        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/proj",
                "app",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();

        assert!(store.delete_app("/proj").unwrap());
        assert!(store.get_app("/proj").unwrap().is_none());
        assert!(!store.delete_app("/proj").unwrap()); // already gone
    }

    #[test]
    fn cleanup_stale_removes_apps_without_tako_toml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = DevStateStore::open(tmp.path().join("db")).unwrap();

        // Create a real project dir with tako.toml
        let real_proj = tmp.path().join("real-proj");
        std::fs::create_dir_all(&real_proj).unwrap();
        std::fs::write(real_proj.join("tako.toml"), "name = \"real\"").unwrap();

        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();

        store
            .register_app(
                real_proj.to_str().unwrap(),
                "real",
                &[],
                3000,
                &AppStatus::Running,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        store
            .register_app(
                "/nonexistent/proj",
                "stale",
                &[],
                3001,
                &AppStatus::Idle,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();

        let removed = store.cleanup_stale().unwrap();
        assert_eq!(removed, vec!["/nonexistent/proj"]);

        let apps = store.list_apps().unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].app_name, "real");
    }

    #[test]
    fn has_any_apps_check() {
        let (_tmp, store) = temp_store();
        assert!(!store.has_any_apps().unwrap());

        let cmd = vec!["bun".to_string()];
        let env = HashMap::new();
        store
            .register_app(
                "/proj",
                "app",
                &[],
                3000,
                &AppStatus::Stopped,
                &cmd,
                &env,
                "/log",
                None,
            )
            .unwrap();
        assert!(store.has_any_apps().unwrap());
    }
}

use crate::instances::AppConfig;
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tako_core::UpgradeMode;

pub const CURRENT_SCHEMA_VERSION: i32 = 2;

#[derive(Debug, Clone)]
pub struct PersistedApp {
    pub config: AppConfig,
    pub routes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    #[error("sqlite error: {0}")]
    Sqlite(String),

    #[error("invalid data: {0}")]
    InvalidData(String),

    #[error("unsupported schema version: {found}")]
    UnsupportedSchemaVersion { found: i32 },
}

impl From<rusqlite::Error> for StateStoreError {
    fn from(e: rusqlite::Error) -> Self {
        StateStoreError::Sqlite(e.to_string())
    }
}

pub struct SqliteStateStore {
    path: PathBuf,
}

impl SqliteStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn init(&self) -> Result<(), StateStoreError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StateStoreError::Sqlite(format!("create db parent: {e}")))?;
        }

        let conn = self.open_connection()?;
        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .map_err(StateStoreError::from)?;

        if version > CURRENT_SCHEMA_VERSION {
            return Err(StateStoreError::UnsupportedSchemaVersion { found: version });
        }

        if version < CURRENT_SCHEMA_VERSION {
            self.migrate(&conn, version)?;
        } else {
            self.ensure_schema_objects(&conn)?;
            self.upsert_schema_meta(&conn)?;
        }

        Ok(())
    }

    pub fn upsert_app(&self, config: &AppConfig, routes: &[String]) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;

        tx.execute(
            "INSERT INTO apps (
                name, version, path, cwd, command_json,
                min_instances, max_instances, base_port, idle_timeout_secs
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(name) DO UPDATE SET
                version = excluded.version,
                path = excluded.path,
                cwd = excluded.cwd,
                command_json = excluded.command_json,
                min_instances = excluded.min_instances,
                max_instances = excluded.max_instances,
                base_port = excluded.base_port,
                idle_timeout_secs = excluded.idle_timeout_secs;",
            rusqlite::params![
                &config.name,
                &config.version,
                config.path.to_string_lossy().as_ref(),
                config.cwd.to_string_lossy().as_ref(),
                serde_json::to_string(&config.command)
                    .map_err(|e| StateStoreError::InvalidData(format!("serialize command: {e}")))?,
                config.min_instances as i64,
                config.max_instances as i64,
                i64::from(config.base_port),
                config.idle_timeout.as_secs() as i64,
            ],
        )
        .map_err(StateStoreError::from)?;

        tx.execute(
            "DELETE FROM app_routes WHERE app_name = ?1;",
            rusqlite::params![&config.name],
        )
        .map_err(StateStoreError::from)?;

        for route in routes {
            tx.execute(
                "INSERT INTO app_routes (app_name, route) VALUES (?1, ?2);",
                rusqlite::params![&config.name, route],
            )
            .map_err(StateStoreError::from)?;
        }

        tx.commit().map_err(StateStoreError::from)?;
        Ok(())
    }

    pub fn delete_app(&self, app_name: &str) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            "DELETE FROM apps WHERE name = ?1;",
            rusqlite::params![app_name],
        )
        .map_err(StateStoreError::from)?;
        Ok(())
    }

    pub fn load_apps(&self) -> Result<Vec<PersistedApp>, StateStoreError> {
        let conn = self.open_connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT
                    name, version, path, cwd, command_json,
                    min_instances, max_instances, base_port, idle_timeout_secs
                 FROM apps
                 ORDER BY name;",
            )
            .map_err(StateStoreError::from)?;

        let mut apps = Vec::new();
        let mut rows = stmt.query([]).map_err(StateStoreError::from)?;

        while let Some(row) = rows.next().map_err(StateStoreError::from)? {
            let name: String = row.get(0).map_err(StateStoreError::from)?;
            let version: String = row.get(1).map_err(StateStoreError::from)?;
            let path_str: String = row.get(2).map_err(StateStoreError::from)?;
            let cwd_str: String = row.get(3).map_err(StateStoreError::from)?;
            let command_json: String = row.get(4).map_err(StateStoreError::from)?;
            let min_instances: i64 = row.get(5).map_err(StateStoreError::from)?;
            let max_instances: i64 = row.get(6).map_err(StateStoreError::from)?;
            let base_port: i64 = row.get(7).map_err(StateStoreError::from)?;
            let idle_timeout_secs: i64 = row.get(8).map_err(StateStoreError::from)?;

            let command: Vec<String> = serde_json::from_str(&command_json)
                .map_err(|e| StateStoreError::InvalidData(format!("deserialize command: {e}")))?;

            let mut routes_stmt = conn
                .prepare("SELECT route FROM app_routes WHERE app_name = ?1 ORDER BY route;")
                .map_err(StateStoreError::from)?;
            let routes: Vec<String> = routes_stmt
                .query_map(rusqlite::params![&name], |r| r.get(0))
                .map_err(StateStoreError::from)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(StateStoreError::from)?;

            let config = AppConfig {
                name,
                version,
                path: PathBuf::from(path_str),
                command,
                cwd: PathBuf::from(cwd_str),
                min_instances: to_u32(min_instances, "min_instances")?,
                max_instances: to_u32(max_instances, "max_instances")?,
                base_port: to_u16(base_port, "base_port")?,
                idle_timeout: Duration::from_secs(to_u64(idle_timeout_secs, "idle_timeout_secs")?),
                ..Default::default()
            };

            apps.push(PersistedApp { config, routes });
        }

        Ok(apps)
    }

    pub fn set_server_mode(&self, mode: UpgradeMode) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            "UPDATE server_state SET server_mode = ?1 WHERE id = 1;",
            rusqlite::params![server_mode_to_str(mode)],
        )
        .map_err(StateStoreError::from)?;
        Ok(())
    }

    pub fn server_mode(&self) -> Result<UpgradeMode, StateStoreError> {
        let conn = self.open_connection()?;
        let mode_str: Option<String> = conn
            .query_row(
                "SELECT server_mode FROM server_state WHERE id = 1;",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StateStoreError::from)?;

        match mode_str {
            Some(s) => server_mode_from_str(&s),
            None => Ok(UpgradeMode::Normal),
        }
    }

    pub fn try_acquire_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.open_connection()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;

        let existing: Option<String> = tx
            .query_row("SELECT owner FROM upgrade_lock WHERE id = 1;", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(StateStoreError::from)?;

        let acquired = match existing {
            Some(existing) if existing == owner => true,
            Some(_) => false,
            None => {
                tx.execute(
                    "INSERT INTO upgrade_lock (id, owner, acquired_at_unix_secs)
                     VALUES (1, ?1, strftime('%s','now'));",
                    rusqlite::params![owner],
                )
                .map_err(StateStoreError::from)?;
                true
            }
        };

        tx.commit().map_err(StateStoreError::from)?;
        Ok(acquired)
    }

    pub fn release_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.open_connection()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;

        let existing: Option<String> = tx
            .query_row("SELECT owner FROM upgrade_lock WHERE id = 1;", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(StateStoreError::from)?;

        let released = match existing {
            Some(existing) if existing == owner => {
                tx.execute("DELETE FROM upgrade_lock WHERE id = 1;", [])
                    .map_err(StateStoreError::from)?;
                true
            }
            _ => false,
        };

        tx.commit().map_err(StateStoreError::from)?;
        Ok(released)
    }

    pub fn upgrade_lock_owner(&self) -> Result<Option<String>, StateStoreError> {
        let conn = self.open_connection()?;
        conn.query_row("SELECT owner FROM upgrade_lock WHERE id = 1;", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(StateStoreError::from)
    }

    fn open_connection(&self) -> Result<rusqlite::Connection, StateStoreError> {
        let conn = rusqlite::Connection::open(&self.path).map_err(StateStoreError::from)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;
             PRAGMA wal_autocheckpoint = 1000;
             PRAGMA journal_size_limit = 67108864;
             PRAGMA cache_size = -20000;
             PRAGMA mmap_size = 134217728;
             PRAGMA trusted_schema = OFF;",
        )
        .map_err(StateStoreError::from)?;
        Ok(conn)
    }

    fn migrate(&self, conn: &rusqlite::Connection, from: i32) -> Result<(), StateStoreError> {
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;
        match from {
            0 => {
                self.ensure_schema_objects_on(&tx)?;
                tx.execute_batch(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION};"))
                    .map_err(StateStoreError::from)?;
                self.upsert_schema_meta_on(&tx)?;
            }
            CURRENT_SCHEMA_VERSION => {}
            other => return Err(StateStoreError::UnsupportedSchemaVersion { found: other }),
        }
        tx.commit().map_err(StateStoreError::from)?;
        Ok(())
    }

    fn ensure_schema_objects(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        self.ensure_schema_objects_on(conn)
    }

    fn ensure_schema_objects_on(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                schema_version INTEGER NOT NULL,
                min_binary_version TEXT NOT NULL,
                created_by TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS apps (
                name TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                path TEXT NOT NULL,
                cwd TEXT NOT NULL,
                command_json TEXT NOT NULL,
                min_instances INTEGER NOT NULL,
                max_instances INTEGER NOT NULL,
                base_port INTEGER NOT NULL,
                idle_timeout_secs INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_routes (
                app_name TEXT NOT NULL,
                route TEXT NOT NULL,
                PRIMARY KEY (app_name, route),
                FOREIGN KEY(app_name) REFERENCES apps(name) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS server_state (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                server_mode TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS upgrade_lock (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                owner TEXT NOT NULL,
                acquired_at_unix_secs INTEGER NOT NULL
            );",
        )
        .map_err(StateStoreError::from)?;
        Ok(())
    }

    fn upsert_schema_meta(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        self.upsert_schema_meta_on(conn)
    }

    fn upsert_schema_meta_on(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        conn.execute(
            "INSERT INTO schema_meta (id, schema_version, min_binary_version, created_by)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                schema_version = excluded.schema_version,
                min_binary_version = excluded.min_binary_version,
                created_by = excluded.created_by;",
            rusqlite::params![
                CURRENT_SCHEMA_VERSION,
                env!("CARGO_PKG_VERSION"),
                "tako-server"
            ],
        )
        .map_err(StateStoreError::from)?;

        conn.execute(
            "INSERT INTO server_state (id, server_mode)
             VALUES (1, 'normal')
             ON CONFLICT(id) DO NOTHING;",
            [],
        )
        .map_err(StateStoreError::from)?;

        Ok(())
    }
}

fn to_u16(value: i64, field: &str) -> Result<u16, StateStoreError> {
    u16::try_from(value).map_err(|_| {
        StateStoreError::InvalidData(format!("field '{field}' out of range for u16: {value}"))
    })
}

fn to_u32(value: i64, field: &str) -> Result<u32, StateStoreError> {
    u32::try_from(value).map_err(|_| {
        StateStoreError::InvalidData(format!("field '{field}' out of range for u32: {value}"))
    })
}

fn to_u64(value: i64, field: &str) -> Result<u64, StateStoreError> {
    u64::try_from(value).map_err(|_| {
        StateStoreError::InvalidData(format!("field '{field}' out of range for u64: {value}"))
    })
}

fn server_mode_to_str(mode: UpgradeMode) -> &'static str {
    match mode {
        UpgradeMode::Normal => "normal",
        UpgradeMode::Upgrading => "upgrading",
    }
}

fn server_mode_from_str(value: &str) -> Result<UpgradeMode, StateStoreError> {
    match value {
        "normal" => Ok(UpgradeMode::Normal),
        "upgrading" => Ok(UpgradeMode::Upgrading),
        other => Err(StateStoreError::InvalidData(format!(
            "unknown server_mode value: {}",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, SqliteStateStore) {
        let temp = TempDir::new().unwrap();
        let store = SqliteStateStore::new(temp.path().join("runtime-state.sqlite3"));
        (temp, store)
    }

    fn sample_config() -> AppConfig {
        AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: PathBuf::from("/opt/tako/apps/my-app/releases/v1"),
            cwd: PathBuf::from("/opt/tako/apps/my-app/releases/v1"),
            command: vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()],
            min_instances: 2,
            max_instances: 4,
            base_port: 4100,
            idle_timeout: Duration::from_secs(3600),
            ..Default::default()
        }
    }

    #[test]
    fn init_creates_schema_and_meta() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        let conn = store.open_connection().unwrap();
        let user_version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, CURRENT_SCHEMA_VERSION);

        let min_binary: String = conn
            .query_row(
                "SELECT min_binary_version FROM schema_meta WHERE id = 1;",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!min_binary.is_empty());
    }

    #[test]
    fn init_rejects_newer_unknown_schema() {
        let (_temp, store) = temp_store();
        let conn = store.open_connection().unwrap();
        conn.execute_batch("PRAGMA user_version = 999;").unwrap();
        drop(conn);

        let err = store.init().unwrap_err();
        match err {
            StateStoreError::UnsupportedSchemaVersion { found } => assert_eq!(found, 999),
            _ => panic!("unexpected error: {err}"),
        }
    }

    #[test]
    fn upsert_and_load_round_trip() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        let cfg = sample_config();
        let routes = vec![
            "api.example.com".to_string(),
            "example.com/api/*".to_string(),
        ];
        store.upsert_app(&cfg, &routes).unwrap();

        let apps = store.load_apps().unwrap();
        assert_eq!(apps.len(), 1);

        let app = &apps[0];
        assert_eq!(app.config.name, "my-app");
        assert_eq!(app.config.version, "v1");
        assert_eq!(
            app.config.path,
            PathBuf::from("/opt/tako/apps/my-app/releases/v1")
        );
        assert_eq!(
            app.config.cwd,
            PathBuf::from("/opt/tako/apps/my-app/releases/v1")
        );
        assert_eq!(
            app.config.command,
            vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()]
        );
        // env_vars and secrets are loaded from files by the caller after restore
        assert!(app.config.env_vars.is_empty());
        assert!(app.config.secrets.is_empty());
        assert_eq!(app.config.min_instances, 2);
        assert_eq!(app.config.max_instances, 4);
        assert_eq!(app.config.base_port, 4100);
        assert_eq!(app.config.idle_timeout, Duration::from_secs(3600));
        assert_eq!(
            app.routes,
            vec![
                "api.example.com".to_string(),
                "example.com/api/*".to_string()
            ]
        );
    }

    #[test]
    fn delete_app_removes_persisted_app() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        let cfg = sample_config();
        let routes = vec!["api.example.com".to_string()];
        store.upsert_app(&cfg, &routes).unwrap();

        store.delete_app("my-app").unwrap();

        let apps = store.load_apps().unwrap();
        assert!(apps.is_empty());
    }

    #[test]
    fn server_mode_defaults_to_normal() {
        let (_temp, store) = temp_store();
        store.init().unwrap();
        assert_eq!(store.server_mode().unwrap(), UpgradeMode::Normal);
    }

    #[test]
    fn server_mode_round_trip_persists() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        store.set_server_mode(UpgradeMode::Upgrading).unwrap();
        assert_eq!(store.server_mode().unwrap(), UpgradeMode::Upgrading);

        // Verify persistence across new connection/process.
        let reopened = SqliteStateStore::new(store.path().to_path_buf());
        reopened.init().unwrap();
        assert_eq!(reopened.server_mode().unwrap(), UpgradeMode::Upgrading);

        reopened.set_server_mode(UpgradeMode::Normal).unwrap();
        assert_eq!(reopened.server_mode().unwrap(), UpgradeMode::Normal);
    }

    #[test]
    fn upgrade_lock_is_single_owner() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        assert!(store.try_acquire_upgrade_lock("controller-a").unwrap());
        assert!(!store.try_acquire_upgrade_lock("controller-b").unwrap());
        assert!(store.try_acquire_upgrade_lock("controller-a").unwrap());
    }

    #[test]
    fn upgrade_lock_release_requires_owner() {
        let (_temp, store) = temp_store();
        store.init().unwrap();
        assert!(store.try_acquire_upgrade_lock("controller-a").unwrap());

        assert!(!store.release_upgrade_lock("controller-b").unwrap());
        assert!(store.release_upgrade_lock("controller-a").unwrap());
        assert!(store.try_acquire_upgrade_lock("controller-b").unwrap());
    }

    #[test]
    fn init_rejects_v1_schema() {
        let (_temp, store) = temp_store();
        // Bootstrap a v1 schema with env_json column
        let conn = store.open_connection().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS apps (
                name TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                path TEXT NOT NULL,
                cwd TEXT NOT NULL,
                command_json TEXT NOT NULL,
                env_json TEXT NOT NULL,
                min_instances INTEGER NOT NULL,
                max_instances INTEGER NOT NULL,
                base_port INTEGER NOT NULL,
                idle_timeout_secs INTEGER NOT NULL
            );",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO apps VALUES ('my-app','v1','/opt/tako','/opt/tako','[\"bun\",\"index.ts\"]','{\"KEY\":\"val\"}',1,4,3000,300);",
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
        drop(conn);

        let err = store.init().unwrap_err();
        match err {
            StateStoreError::UnsupportedSchemaVersion { found } => assert_eq!(found, 1),
            _ => panic!("unexpected error: {err}"),
        }
    }
}

use crate::instances::AppConfig;
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};
use tako_core::UpgradeMode;

pub const STATE_SCHEMA_VERSION: i32 = 1;

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

        if version > STATE_SCHEMA_VERSION {
            return Err(StateStoreError::UnsupportedSchemaVersion { found: version });
        }

        if version == 0 {
            self.initialize_schema(&conn)?;
        } else if version == STATE_SCHEMA_VERSION {
            self.ensure_schema_objects(&conn)?;
            self.ensure_default_rows(&conn)?;
        } else {
            return Err(StateStoreError::UnsupportedSchemaVersion { found: version });
        }

        Ok(())
    }

    pub fn upsert_app(&self, config: &AppConfig, routes: &[String]) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;
        upsert_app_on(&tx, config, routes)?;

        tx.commit().map_err(StateStoreError::from)?;
        Ok(())
    }

    pub fn delete_app(&self, name: &str, environment: &str) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        conn.execute(
            "DELETE FROM apps WHERE name = ?1 AND environment = ?2;",
            rusqlite::params![name, environment],
        )
        .map_err(StateStoreError::from)?;
        Ok(())
    }

    pub fn load_apps(&self) -> Result<Vec<PersistedApp>, StateStoreError> {
        let conn = self.open_connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT
                    name, environment, version, app_subdir, min_instances, max_instances
                 FROM apps
                 ORDER BY name, environment;",
            )
            .map_err(StateStoreError::from)?;

        let mut apps = Vec::new();
        let mut rows = stmt.query([]).map_err(StateStoreError::from)?;

        while let Some(row) = rows.next().map_err(StateStoreError::from)? {
            let name: String = row.get(0).map_err(StateStoreError::from)?;
            let environment: String = row.get(1).map_err(StateStoreError::from)?;
            let version: String = row.get(2).map_err(StateStoreError::from)?;
            let app_subdir: String = row.get(3).map_err(StateStoreError::from)?;
            let min_instances: i64 = row.get(4).map_err(StateStoreError::from)?;
            let max_instances: i64 = row.get(5).map_err(StateStoreError::from)?;

            let mut routes_stmt = conn
                .prepare(
                    "SELECT route FROM app_routes
                     WHERE name = ?1 AND environment = ?2
                     ORDER BY route;",
                )
                .map_err(StateStoreError::from)?;
            let routes: Vec<String> = routes_stmt
                .query_map(rusqlite::params![&name, &environment], |r| r.get(0))
                .map_err(StateStoreError::from)?
                .collect::<Result<Vec<String>, _>>()
                .map_err(StateStoreError::from)?;

            let config = AppConfig {
                name,
                environment,
                version,
                app_subdir,
                min_instances: to_u32(min_instances, "min_instances")?,
                max_instances: to_u32(max_instances, "max_instances")?,
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

    /// Stale lock threshold: locks older than this are force-acquired.
    const UPGRADE_LOCK_STALE_SECS: i64 = 600; // 10 minutes

    pub fn try_acquire_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.open_connection()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;

        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT owner, acquired_at_unix_secs FROM upgrade_lock WHERE id = 1;",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StateStoreError::from)?;

        let now: i64 = tx
            .query_row("SELECT CAST(strftime('%s','now') AS INTEGER);", [], |row| {
                row.get(0)
            })
            .map_err(StateStoreError::from)?;

        let acquired = match existing {
            Some((ref existing_owner, _)) if existing_owner == owner => true,
            Some((_, acquired_at)) if now - acquired_at > Self::UPGRADE_LOCK_STALE_SECS => {
                // Stale lock — force-acquire by replacing it.
                tx.execute(
                    "UPDATE upgrade_lock SET owner = ?1, acquired_at_unix_secs = ?2 WHERE id = 1;",
                    rusqlite::params![owner, now],
                )
                .map_err(StateStoreError::from)?;
                true
            }
            Some(_) => false,
            None => {
                tx.execute(
                    "INSERT INTO upgrade_lock (id, owner, acquired_at_unix_secs)
                     VALUES (1, ?1, CAST(strftime('%s','now') AS INTEGER));",
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

    fn ensure_schema_objects(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        self.ensure_schema_objects_on(conn)
    }

    fn initialize_schema(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        let tx = conn
            .unchecked_transaction()
            .map_err(StateStoreError::from)?;
        self.ensure_schema_objects_on(&tx)?;
        self.ensure_default_rows_on(&tx)?;
        tx.execute_batch(&format!("PRAGMA user_version = {STATE_SCHEMA_VERSION};"))
            .map_err(StateStoreError::from)?;
        tx.commit().map_err(StateStoreError::from)?;
        Ok(())
    }

    fn ensure_schema_objects_on(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS apps (
                name TEXT NOT NULL,
                environment TEXT NOT NULL,
                version TEXT NOT NULL,
                app_subdir TEXT NOT NULL,
                min_instances INTEGER NOT NULL,
                max_instances INTEGER NOT NULL,
                PRIMARY KEY (name, environment)
            );

            CREATE TABLE IF NOT EXISTS app_routes (
                name TEXT NOT NULL,
                environment TEXT NOT NULL,
                route TEXT NOT NULL,
                PRIMARY KEY (name, environment, route),
                FOREIGN KEY(name, environment) REFERENCES apps(name, environment) ON DELETE CASCADE
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

    fn ensure_default_rows(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
        self.ensure_default_rows_on(conn)
    }

    fn ensure_default_rows_on(&self, conn: &rusqlite::Connection) -> Result<(), StateStoreError> {
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

fn upsert_app_on(
    conn: &rusqlite::Connection,
    config: &AppConfig,
    routes: &[String],
) -> Result<(), StateStoreError> {
    conn.execute(
        "INSERT INTO apps (
            name, environment, version, app_subdir, min_instances, max_instances
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(name, environment) DO UPDATE SET
            version = excluded.version,
            app_subdir = excluded.app_subdir,
            min_instances = excluded.min_instances,
            max_instances = excluded.max_instances;",
        rusqlite::params![
            &config.name,
            &config.environment,
            &config.version,
            &config.app_subdir,
            config.min_instances as i64,
            config.max_instances as i64,
        ],
    )
    .map_err(StateStoreError::from)?;

    conn.execute(
        "DELETE FROM app_routes WHERE name = ?1 AND environment = ?2;",
        rusqlite::params![&config.name, &config.environment],
    )
    .map_err(StateStoreError::from)?;

    for route in routes {
        conn.execute(
            "INSERT INTO app_routes (name, environment, route) VALUES (?1, ?2, ?3);",
            rusqlite::params![&config.name, &config.environment, route],
        )
        .map_err(StateStoreError::from)?;
    }

    Ok(())
}

fn to_u32(value: i64, field: &str) -> Result<u32, StateStoreError> {
    u32::try_from(value).map_err(|_| {
        StateStoreError::InvalidData(format!("field '{field}' out of range for u32: {value}"))
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
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, SqliteStateStore) {
        let temp = TempDir::new().unwrap();
        let store = SqliteStateStore::new(temp.path().join("runtime-state.sqlite3"));
        (temp, store)
    }

    fn sample_config() -> AppConfig {
        AppConfig {
            name: "my-app".to_string(),
            environment: "production".to_string(),
            version: "v1".to_string(),
            app_subdir: "examples/bun".to_string(),
            min_instances: 2,
            max_instances: 4,
            ..Default::default()
        }
    }

    #[test]
    fn init_creates_schema() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        let conn = store.open_connection().unwrap();
        let user_version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(user_version, STATE_SCHEMA_VERSION);

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
                "name".to_string(),
                "environment".to_string(),
                "version".to_string(),
                "app_subdir".to_string(),
                "min_instances".to_string(),
                "max_instances".to_string(),
            ]
        );
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
        assert_eq!(app.config.environment, "production");
        assert_eq!(app.config.version, "v1");
        assert_eq!(app.config.app_subdir, "examples/bun");
        // env_vars and secrets are loaded from files by the caller after restore
        assert!(app.config.env_vars.is_empty());
        assert!(app.config.secrets.is_empty());
        assert_eq!(app.config.min_instances, 2);
        assert_eq!(app.config.max_instances, 4);
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

        store.delete_app("my-app", "production").unwrap();

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
    fn upgrade_lock_force_acquires_stale_lock() {
        let (_temp, store) = temp_store();
        store.init().unwrap();
        assert!(store.try_acquire_upgrade_lock("controller-a").unwrap());

        // Backdate the lock to make it stale.
        let conn = store.open_connection().unwrap();
        let stale_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - SqliteStateStore::UPGRADE_LOCK_STALE_SECS
            - 1;
        conn.execute(
            "UPDATE upgrade_lock SET acquired_at_unix_secs = ?1 WHERE id = 1;",
            rusqlite::params![stale_time],
        )
        .unwrap();

        // A different owner can now force-acquire the stale lock.
        assert!(store.try_acquire_upgrade_lock("controller-b").unwrap());
        assert_eq!(
            store.upgrade_lock_owner().unwrap().as_deref(),
            Some("controller-b")
        );
    }

    #[test]
    fn upgrade_lock_owner_cleared_allows_new_owner() {
        let (_temp, store) = temp_store();
        store.init().unwrap();

        // Simulate: owner-a acquires lock then crashes (no release).
        assert!(store.try_acquire_upgrade_lock("owner-a").unwrap());
        assert_eq!(
            store.upgrade_lock_owner().unwrap().as_deref(),
            Some("owner-a")
        );

        // Manual cleanup (as server startup would do): read owner, release.
        if let Some(owner) = store.upgrade_lock_owner().unwrap() {
            assert!(store.release_upgrade_lock(&owner).unwrap());
        }

        // New owner can acquire immediately without waiting for stale timeout.
        assert!(store.try_acquire_upgrade_lock("owner-b").unwrap());
        assert_eq!(
            store.upgrade_lock_owner().unwrap().as_deref(),
            Some("owner-b")
        );
    }
}

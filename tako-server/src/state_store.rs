use crate::instances::AppConfig;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::ptr::{self, null_mut};
use std::time::Duration;
use tako_core::UpgradeMode;

pub const CURRENT_SCHEMA_VERSION: i32 = 1;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;

#[repr(C)]
struct Sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct Sqlite3Stmt {
    _private: [u8; 0],
}

type SqliteDestructor = Option<unsafe extern "C" fn(*mut c_void)>;

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open(filename: *const c_char, pp_db: *mut *mut Sqlite3) -> c_int;
    fn sqlite3_close(db: *mut Sqlite3) -> c_int;
    fn sqlite3_exec(
        db: *mut Sqlite3,
        sql: *const c_char,
        callback: Option<unsafe extern "C" fn()>,
        arg: *mut c_void,
        errmsg: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(ptr: *mut c_void);
    fn sqlite3_errmsg(db: *mut Sqlite3) -> *const c_char;
    fn sqlite3_prepare_v2(
        db: *mut Sqlite3,
        sql: *const c_char,
        n_byte: c_int,
        pp_stmt: *mut *mut Sqlite3Stmt,
        pz_tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_finalize(stmt: *mut Sqlite3Stmt) -> c_int;
    fn sqlite3_bind_text(
        stmt: *mut Sqlite3Stmt,
        index: c_int,
        value: *const c_char,
        n: c_int,
        destructor: SqliteDestructor,
    ) -> c_int;
    fn sqlite3_bind_int(stmt: *mut Sqlite3Stmt, index: c_int, value: c_int) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut Sqlite3Stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_column_int(stmt: *mut Sqlite3Stmt, i_col: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut Sqlite3Stmt, i_col: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut Sqlite3Stmt, i_col: c_int) -> *const u8;
}

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

struct RawConnection {
    db: *mut Sqlite3,
}

impl RawConnection {
    fn open(path: &Path) -> Result<Self, StateStoreError> {
        let mut db: *mut Sqlite3 = null_mut();
        let c_path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
        let rc = unsafe { sqlite3_open(c_path.as_ptr(), &mut db) };
        if rc != SQLITE_OK {
            let message = if db.is_null() {
                "failed to open sqlite database".to_string()
            } else {
                unsafe { c_str_from_ptr(sqlite3_errmsg(db)) }
            };
            if !db.is_null() {
                let _ = unsafe { sqlite3_close(db) };
            }
            return Err(StateStoreError::Sqlite(message));
        }
        Ok(Self { db })
    }

    fn exec(&self, sql: &str) -> Result<(), StateStoreError> {
        let c_sql = CString::new(sql).map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
        let mut err_ptr: *mut c_char = null_mut();
        let rc =
            unsafe { sqlite3_exec(self.db, c_sql.as_ptr(), None, ptr::null_mut(), &mut err_ptr) };
        if rc != SQLITE_OK {
            let message = if !err_ptr.is_null() {
                let msg = unsafe { c_str_from_ptr(err_ptr) };
                unsafe { sqlite3_free(err_ptr as *mut c_void) };
                msg
            } else {
                self.errmsg()
            };
            return Err(StateStoreError::Sqlite(message));
        }
        Ok(())
    }

    fn prepare(&self, sql: &str) -> Result<RawStatement, StateStoreError> {
        let c_sql = CString::new(sql).map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
        let mut stmt: *mut Sqlite3Stmt = null_mut();
        let rc =
            unsafe { sqlite3_prepare_v2(self.db, c_sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(StateStoreError::Sqlite(self.errmsg()));
        }
        Ok(RawStatement { db: self.db, stmt })
    }

    fn query_user_version(&self) -> Result<i32, StateStoreError> {
        let mut stmt = self.prepare("PRAGMA user_version;")?;
        match stmt.step()? {
            Step::Row => stmt.column_int(0),
            Step::Done => Ok(0),
        }
    }

    fn errmsg(&self) -> String {
        unsafe { c_str_from_ptr(sqlite3_errmsg(self.db)) }
    }
}

impl Drop for RawConnection {
    fn drop(&mut self) {
        if !self.db.is_null() {
            let _ = unsafe { sqlite3_close(self.db) };
            self.db = null_mut();
        }
    }
}

enum Step {
    Row,
    Done,
}

struct RawStatement {
    db: *mut Sqlite3,
    stmt: *mut Sqlite3Stmt,
}

impl RawStatement {
    fn bind_text(&mut self, index: c_int, value: &str) -> Result<(), StateStoreError> {
        let c = CString::new(value).map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
        let rc = unsafe {
            sqlite3_bind_text(
                self.stmt,
                index,
                c.as_ptr(),
                -1,
                sqlite_transient_destructor(),
            )
        };
        if rc != SQLITE_OK {
            return Err(StateStoreError::Sqlite(self.errmsg()));
        }
        Ok(())
    }

    fn bind_int(&mut self, index: c_int, value: i32) -> Result<(), StateStoreError> {
        let rc = unsafe { sqlite3_bind_int(self.stmt, index, value) };
        if rc != SQLITE_OK {
            return Err(StateStoreError::Sqlite(self.errmsg()));
        }
        Ok(())
    }

    fn bind_int64(&mut self, index: c_int, value: i64) -> Result<(), StateStoreError> {
        let rc = unsafe { sqlite3_bind_int64(self.stmt, index, value) };
        if rc != SQLITE_OK {
            return Err(StateStoreError::Sqlite(self.errmsg()));
        }
        Ok(())
    }

    fn step(&mut self) -> Result<Step, StateStoreError> {
        let rc = unsafe { sqlite3_step(self.stmt) };
        match rc {
            SQLITE_ROW => Ok(Step::Row),
            SQLITE_DONE => Ok(Step::Done),
            _ => Err(StateStoreError::Sqlite(self.errmsg())),
        }
    }

    fn execute_done(&mut self) -> Result<(), StateStoreError> {
        match self.step()? {
            Step::Done => Ok(()),
            Step::Row => Err(StateStoreError::Sqlite(
                "expected statement to complete without rows".to_string(),
            )),
        }
    }

    fn column_int(&self, col: c_int) -> Result<i32, StateStoreError> {
        Ok(unsafe { sqlite3_column_int(self.stmt, col) })
    }

    fn column_int64(&self, col: c_int) -> Result<i64, StateStoreError> {
        Ok(unsafe { sqlite3_column_int64(self.stmt, col) })
    }

    fn column_text(&self, col: c_int) -> Result<String, StateStoreError> {
        let ptr = unsafe { sqlite3_column_text(self.stmt, col) };
        if ptr.is_null() {
            return Ok(String::new());
        }
        Ok(unsafe { c_str_from_u8_ptr(ptr) })
    }

    fn errmsg(&self) -> String {
        unsafe { c_str_from_ptr(sqlite3_errmsg(self.db)) }
    }
}

impl Drop for RawStatement {
    fn drop(&mut self) {
        if !self.stmt.is_null() {
            let _ = unsafe { sqlite3_finalize(self.stmt) };
            self.stmt = null_mut();
        }
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
        let version = conn.query_user_version()?;
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
        conn.exec("BEGIN IMMEDIATE;")?;

        let result = (|| {
            let mut stmt = conn.prepare(
                "INSERT INTO apps (
                    name, version, path, cwd, command_json, env_json,
                    min_instances, max_instances, base_port, idle_timeout_secs
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(name) DO UPDATE SET
                    version = excluded.version,
                    path = excluded.path,
                    cwd = excluded.cwd,
                    command_json = excluded.command_json,
                    env_json = excluded.env_json,
                    min_instances = excluded.min_instances,
                    max_instances = excluded.max_instances,
                    base_port = excluded.base_port,
                    idle_timeout_secs = excluded.idle_timeout_secs;",
            )?;
            stmt.bind_text(1, &config.name)?;
            stmt.bind_text(2, &config.version)?;
            stmt.bind_text(3, config.path.to_string_lossy().as_ref())?;
            stmt.bind_text(4, config.cwd.to_string_lossy().as_ref())?;
            let command_json = serde_json::to_string(&config.command)
                .map_err(|e| StateStoreError::InvalidData(format!("serialize command: {e}")))?;
            let env_json = serde_json::to_string(&config.env)
                .map_err(|e| StateStoreError::InvalidData(format!("serialize env: {e}")))?;
            stmt.bind_text(5, &command_json)?;
            stmt.bind_text(6, &env_json)?;
            stmt.bind_int64(7, config.min_instances as i64)?;
            stmt.bind_int64(8, config.max_instances as i64)?;
            stmt.bind_int64(9, i64::from(config.base_port))?;
            stmt.bind_int64(10, config.idle_timeout.as_secs() as i64)?;
            stmt.execute_done()?;

            let mut clear_routes = conn.prepare("DELETE FROM app_routes WHERE app_name = ?1;")?;
            clear_routes.bind_text(1, &config.name)?;
            clear_routes.execute_done()?;

            for route in routes {
                let mut insert_route =
                    conn.prepare("INSERT INTO app_routes (app_name, route) VALUES (?1, ?2);")?;
                insert_route.bind_text(1, &config.name)?;
                insert_route.bind_text(2, route)?;
                insert_route.execute_done()?;
            }

            Ok(())
        })();

        match result {
            Ok(()) => conn.exec("COMMIT;"),
            Err(err) => {
                let _ = conn.exec("ROLLBACK;");
                Err(err)
            }
        }
    }

    pub fn delete_app(&self, app_name: &str) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        let mut delete = conn.prepare("DELETE FROM apps WHERE name = ?1;")?;
        delete.bind_text(1, app_name)?;
        delete.execute_done()?;
        Ok(())
    }

    pub fn load_apps(&self) -> Result<Vec<PersistedApp>, StateStoreError> {
        let conn = self.open_connection()?;

        let mut stmt = conn.prepare(
            "SELECT
                name, version, path, cwd, command_json, env_json,
                min_instances, max_instances, base_port, idle_timeout_secs
             FROM apps
             ORDER BY name;",
        )?;

        let mut apps = Vec::new();
        while let Step::Row = stmt.step()? {
            let name = stmt.column_text(0)?;
            let version = stmt.column_text(1)?;
            let path = PathBuf::from(stmt.column_text(2)?);
            let cwd = PathBuf::from(stmt.column_text(3)?);
            let command_json = stmt.column_text(4)?;
            let env_json = stmt.column_text(5)?;
            let min_instances = to_u32(stmt.column_int64(6)?, "min_instances")?;
            let max_instances = to_u32(stmt.column_int64(7)?, "max_instances")?;
            let base_port = to_u16(stmt.column_int64(8)?, "base_port")?;
            let idle_timeout_secs = to_u64(stmt.column_int64(9)?, "idle_timeout_secs")?;

            let command: Vec<String> = serde_json::from_str(&command_json)
                .map_err(|e| StateStoreError::InvalidData(format!("deserialize command: {e}")))?;
            let env: HashMap<String, String> = serde_json::from_str(&env_json)
                .map_err(|e| StateStoreError::InvalidData(format!("deserialize env: {e}")))?;

            let mut routes_stmt =
                conn.prepare("SELECT route FROM app_routes WHERE app_name = ?1 ORDER BY route;")?;
            routes_stmt.bind_text(1, &name)?;
            let mut routes = Vec::new();
            while let Step::Row = routes_stmt.step()? {
                routes.push(routes_stmt.column_text(0)?);
            }

            let config = AppConfig {
                name,
                version,
                path,
                command,
                cwd,
                env,
                min_instances,
                max_instances,
                base_port,
                idle_timeout: Duration::from_secs(idle_timeout_secs),
                ..Default::default()
            };

            apps.push(PersistedApp { config, routes });
        }

        Ok(apps)
    }

    pub fn set_server_mode(&self, mode: UpgradeMode) -> Result<(), StateStoreError> {
        let conn = self.open_connection()?;
        let mut stmt = conn.prepare(
            "UPDATE server_state
             SET server_mode = ?1
             WHERE id = 1;",
        )?;
        stmt.bind_text(1, server_mode_to_str(mode))?;
        stmt.execute_done()?;
        Ok(())
    }

    pub fn server_mode(&self) -> Result<UpgradeMode, StateStoreError> {
        let conn = self.open_connection()?;
        let mut stmt = conn.prepare("SELECT server_mode FROM server_state WHERE id = 1;")?;
        match stmt.step()? {
            Step::Row => server_mode_from_str(&stmt.column_text(0)?),
            Step::Done => Ok(UpgradeMode::Normal),
        }
    }

    pub fn try_acquire_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.open_connection()?;
        conn.exec("BEGIN IMMEDIATE;")?;
        let result = (|| match self.upgrade_lock_owner_on_conn(&conn)? {
            Some(existing) if existing == owner => Ok(true),
            Some(_) => Ok(false),
            None => {
                let mut stmt = conn.prepare(
                    "INSERT INTO upgrade_lock (id, owner, acquired_at_unix_secs)
                         VALUES (1, ?1, strftime('%s','now'));",
                )?;
                stmt.bind_text(1, owner)?;
                stmt.execute_done()?;
                Ok(true)
            }
        })();
        match result {
            Ok(acquired) => {
                conn.exec("COMMIT;")?;
                Ok(acquired)
            }
            Err(err) => {
                let _ = conn.exec("ROLLBACK;");
                Err(err)
            }
        }
    }

    pub fn release_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.open_connection()?;
        conn.exec("BEGIN IMMEDIATE;")?;
        let result = (|| match self.upgrade_lock_owner_on_conn(&conn)? {
            Some(existing) if existing == owner => {
                let mut stmt = conn.prepare("DELETE FROM upgrade_lock WHERE id = 1;")?;
                stmt.execute_done()?;
                Ok(true)
            }
            _ => Ok(false),
        })();
        match result {
            Ok(released) => {
                conn.exec("COMMIT;")?;
                Ok(released)
            }
            Err(err) => {
                let _ = conn.exec("ROLLBACK;");
                Err(err)
            }
        }
    }

    pub fn upgrade_lock_owner(&self) -> Result<Option<String>, StateStoreError> {
        let conn = self.open_connection()?;
        self.upgrade_lock_owner_on_conn(&conn)
    }

    fn open_connection(&self) -> Result<RawConnection, StateStoreError> {
        let conn = RawConnection::open(&self.path)?;
        conn.exec("PRAGMA journal_mode = WAL;")?;
        conn.exec("PRAGMA synchronous = NORMAL;")?;
        conn.exec("PRAGMA foreign_keys = ON;")?;
        conn.exec("PRAGMA busy_timeout = 5000;")?;
        conn.exec("PRAGMA temp_store = MEMORY;")?;
        conn.exec("PRAGMA wal_autocheckpoint = 1000;")?;
        conn.exec("PRAGMA journal_size_limit = 67108864;")?;
        conn.exec("PRAGMA cache_size = -20000;")?;
        conn.exec("PRAGMA mmap_size = 134217728;")?;
        conn.exec("PRAGMA trusted_schema = OFF;")?;
        Ok(conn)
    }

    fn migrate(&self, conn: &RawConnection, from: i32) -> Result<(), StateStoreError> {
        conn.exec("BEGIN IMMEDIATE;")?;
        let result = (|| {
            match from {
                0 => {
                    self.ensure_schema_objects(conn)?;
                    conn.exec(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION};"))?;
                    self.upsert_schema_meta(conn)?;
                }
                CURRENT_SCHEMA_VERSION => {}
                other => return Err(StateStoreError::UnsupportedSchemaVersion { found: other }),
            }
            Ok(())
        })();

        match result {
            Ok(()) => conn.exec("COMMIT;"),
            Err(err) => {
                let _ = conn.exec("ROLLBACK;");
                Err(err)
            }
        }
    }

    fn ensure_schema_objects(&self, conn: &RawConnection) -> Result<(), StateStoreError> {
        conn.exec(
            "CREATE TABLE IF NOT EXISTS schema_meta (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                schema_version INTEGER NOT NULL,
                min_binary_version TEXT NOT NULL,
                created_by TEXT NOT NULL
            );",
        )?;

        conn.exec(
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
        )?;

        conn.exec(
            "CREATE TABLE IF NOT EXISTS app_routes (
                app_name TEXT NOT NULL,
                route TEXT NOT NULL,
                PRIMARY KEY (app_name, route),
                FOREIGN KEY(app_name) REFERENCES apps(name) ON DELETE CASCADE
            );",
        )?;

        conn.exec(
            "CREATE TABLE IF NOT EXISTS server_state (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                server_mode TEXT NOT NULL
            );",
        )?;

        conn.exec(
            "CREATE TABLE IF NOT EXISTS upgrade_lock (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                owner TEXT NOT NULL,
                acquired_at_unix_secs INTEGER NOT NULL
            );",
        )?;

        Ok(())
    }

    fn upsert_schema_meta(&self, conn: &RawConnection) -> Result<(), StateStoreError> {
        let mut stmt = conn.prepare(
            "INSERT INTO schema_meta (id, schema_version, min_binary_version, created_by)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                schema_version = excluded.schema_version,
                min_binary_version = excluded.min_binary_version,
                created_by = excluded.created_by;",
        )?;
        stmt.bind_int(1, CURRENT_SCHEMA_VERSION)?;
        stmt.bind_text(2, env!("CARGO_PKG_VERSION"))?;
        stmt.bind_text(3, "tako-server")?;
        stmt.execute_done()?;

        let mut mode_stmt = conn.prepare(
            "INSERT INTO server_state (id, server_mode)
             VALUES (1, 'normal')
             ON CONFLICT(id) DO NOTHING;",
        )?;
        mode_stmt.execute_done()?;

        Ok(())
    }

    fn upgrade_lock_owner_on_conn(
        &self,
        conn: &RawConnection,
    ) -> Result<Option<String>, StateStoreError> {
        let mut stmt = conn.prepare("SELECT owner FROM upgrade_lock WHERE id = 1;")?;
        match stmt.step()? {
            Step::Row => Ok(Some(stmt.column_text(0)?)),
            Step::Done => Ok(None),
        }
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

unsafe fn c_str_from_ptr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
}

unsafe fn c_str_from_u8_ptr(ptr: *const u8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr as *const c_char) }
        .to_string_lossy()
        .to_string()
}

fn sqlite_transient_destructor() -> SqliteDestructor {
    unsafe { std::mem::transmute::<isize, SqliteDestructor>(-1) }
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
        let mut env = HashMap::new();
        env.insert("DATABASE_URL".to_string(), "postgres://db".to_string());

        AppConfig {
            name: "my-app".to_string(),
            version: "v1".to_string(),
            path: PathBuf::from("/opt/tako/apps/my-app/releases/v1"),
            cwd: PathBuf::from("/opt/tako/apps/my-app/releases/v1"),
            command: vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()],
            env,
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
        let user_version = conn.query_user_version().unwrap();
        assert_eq!(user_version, CURRENT_SCHEMA_VERSION);

        let mut stmt = conn
            .prepare("SELECT min_binary_version FROM schema_meta WHERE id = 1;")
            .unwrap();
        match stmt.step().unwrap() {
            Step::Row => {
                let min_binary = stmt.column_text(0).unwrap();
                assert!(!min_binary.is_empty());
            }
            Step::Done => panic!("expected schema_meta row"),
        }
    }

    #[test]
    fn init_rejects_newer_unknown_schema() {
        let (_temp, store) = temp_store();
        let conn = store.open_connection().unwrap();
        conn.exec("PRAGMA user_version = 999;").unwrap();
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
        assert_eq!(
            app.config.env.get("DATABASE_URL"),
            Some(&"postgres://db".to_string())
        );
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
}

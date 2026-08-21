//! Shared rusqlite open helpers for Tako's SQLite-backed stores.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

/// How long a connection waits on a locked database before erroring.
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Open (or create) a file-backed SQLite database with Tako's standard
/// settings: WAL (so the old and new server processes can hold the same DB
/// during a zero-downtime reload), a busy timeout, synchronous NORMAL, and
/// foreign keys on.
pub fn open_local(path: impl AsRef<Path>) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Open an in-memory SQLite database (single-process; WAL does not apply).
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pragma_string(conn: &Connection, name: &str) -> String {
        conn.pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    fn pragma_i64(conn: &Connection, name: &str) -> i64 {
        conn.pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn open_local_enables_wal_foreign_keys_and_busy_timeout() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tako.sqlite");
        let conn = open_local(&path).unwrap();
        assert_eq!(pragma_string(&conn, "journal_mode").to_lowercase(), "wal");
        assert_eq!(pragma_i64(&conn, "synchronous"), 1); // NORMAL
        assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
        assert_eq!(pragma_i64(&conn, "busy_timeout"), 5000);
    }

    #[test]
    fn open_in_memory_enables_foreign_keys() {
        let conn = open_in_memory().unwrap();
        assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
        assert_eq!(pragma_i64(&conn, "busy_timeout"), 5000);
    }

    #[test]
    fn rusqlite_transaction_rollback_on_drop_releases_write_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tako.sqlite");
        let mut writer = open_local(&path).unwrap();
        writer.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        {
            let tx = writer.transaction().unwrap();
            tx.execute("INSERT INTO t (x) VALUES (1)", []).unwrap();
            // Drop without commit — rusqlite rolls back on Drop.
        }
        let reader = open_local(&path).unwrap();
        reader.execute("INSERT INTO t (x) VALUES (2)", []).unwrap();
        let count: i64 = reader
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}

//! Run enqueue + per-step persistence + lifecycle transitions.
//!
//! All run state lives in two tables:
//!   - `runs` — one row per run; tracks status, attempts, lease.
//!   - `steps` — append-only memoization of completed step results.
//!
//! Operations are synchronous; call from `tokio::task::spawn_blocking` when
//! invoking from async contexts.

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tako_core::{EnqueueOpts, EnqueueRunResponse, RunPayload};

use super::schema;

const DEFAULT_MAX_ATTEMPTS: u32 = 3;

#[derive(thiserror::Error, Debug)]
pub enum RunsDbError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct RunsDb {
    conn: Mutex<Connection>,
}

impl RunsDb {
    pub fn open(path: &Path) -> Result<Self, RunsDbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                RunsDbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })?;
        }
        let conn = Connection::open(path)?;
        schema::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self, RunsDbError> {
        let conn = Connection::open_in_memory()?;
        schema::init(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Insert a new run, or return the id of an existing non-terminal run
    /// with the same `unique_key` if one exists.
    pub fn enqueue(
        &self,
        name: &str,
        payload: &serde_json::Value,
        opts: &EnqueueOpts,
    ) -> Result<EnqueueRunResponse, RunsDbError> {
        let now_ms = now_ms();
        let run_at = opts.run_at_ms.unwrap_or(now_ms);
        let max_attempts = opts.max_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS) as i64;
        let unique_key = opts.unique_key.as_deref();
        let payload_json = serde_json::to_string(payload)?;

        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        if let Some(key) = unique_key {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM runs WHERE unique_key = ?1 AND status IN ('pending','running') LIMIT 1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(id) = existing {
                tx.commit()?;
                return Ok(EnqueueRunResponse {
                    id,
                    deduplicated: true,
                });
            }
        }

        let id = nanoid::nanoid!();
        tx.execute(
            "INSERT INTO runs
             (id, name, payload, status, attempts, max_attempts, run_at, lease_until, worker_id,
              last_error, created_at, unique_key)
             VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?5, NULL, NULL, NULL, ?6, ?7)",
            params![
                id,
                name,
                payload_json,
                max_attempts,
                run_at,
                now_ms,
                unique_key
            ],
        )?;
        tx.commit()?;

        Ok(EnqueueRunResponse {
            id,
            deduplicated: false,
        })
    }

    pub(crate) fn lock_conn(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    /// Atomically claim the oldest eligible run for one of `names`. Bumps
    /// `attempts`. Returns `None` when nothing is due. Loads any persisted
    /// step results into `step_state`.
    pub fn claim(
        &self,
        worker_id: &str,
        names: &[String],
        lease_ms: u64,
    ) -> Result<Option<RunPayload>, RunsDbError> {
        if names.is_empty() {
            return Ok(None);
        }
        let now = now_ms();
        let lease_until = now + lease_ms as i64;
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE runs
             SET status='running', worker_id=?, lease_until=?, attempts=attempts+1
             WHERE id = (
                 SELECT id FROM runs
                 WHERE status='pending' AND run_at <= ? AND name IN ({})
                 ORDER BY run_at
                 LIMIT 1
             )
             RETURNING id, name, payload, status, attempts, max_attempts, run_at",
            placeholders
        );
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(3 + names.len());
        params.push(Box::new(worker_id.to_string()));
        params.push(Box::new(lease_until));
        params.push(Box::new(now));
        for n in names {
            params.push(Box::new(n.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let row_opt = stmt
            .query_row(&refs[..], |row| {
                let payload: String = row.get(2)?;
                Ok(RunPayload {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    status: row.get(3)?,
                    attempts: row.get::<_, i64>(4)? as u32,
                    max_attempts: row.get::<_, i64>(5)? as u32,
                    run_at_ms: row.get(6)?,
                    step_state: serde_json::Value::Object(Default::default()),
                })
            })
            .optional()?;

        let Some(mut payload) = row_opt else {
            return Ok(None);
        };

        // Hydrate persisted step results into step_state.
        let mut step_stmt = conn.prepare("SELECT name, result FROM steps WHERE run_id = ?1")?;
        let mut state_map = serde_json::Map::new();
        let rows = step_stmt.query_map(params![payload.id], |row| {
            let name: String = row.get(0)?;
            let result: String = row.get(1)?;
            Ok((name, result))
        })?;
        for r in rows {
            let (name, result) = r?;
            let value = serde_json::from_str(&result).unwrap_or(serde_json::Value::Null);
            state_map.insert(name, value);
        }
        payload.step_state = serde_json::Value::Object(state_map);

        Ok(Some(payload))
    }

    pub fn heartbeat(&self, id: &str, lease_ms: u64) -> Result<(), RunsDbError> {
        let lease_until = now_ms() + lease_ms as i64;
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE runs SET lease_until = ?1 WHERE id = ?2 AND status='running'",
            params![lease_until, id],
        )?;
        Ok(())
    }

    /// Persist a single completed step result. First-write-wins on the
    /// `(run_id, name)` primary key — a duplicate save (e.g. if the worker
    /// retried after a failed RPC) doesn't overwrite the original.
    pub fn save_step(
        &self,
        run_id: &str,
        step_name: &str,
        result: &serde_json::Value,
    ) -> Result<(), RunsDbError> {
        let r = serde_json::to_string(result)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO steps (run_id, name, result, completed_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id, step_name, r, now_ms()],
        )?;
        Ok(())
    }

    pub fn complete(&self, id: &str) -> Result<(), RunsDbError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE runs SET status='succeeded', worker_id=NULL, lease_until=NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn cancel(&self, id: &str, reason: Option<&str>) -> Result<(), RunsDbError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE runs SET status='cancelled', last_error=?1, worker_id=NULL, lease_until=NULL WHERE id = ?2",
            params![reason, id],
        )?;
        Ok(())
    }

    pub fn fail(
        &self,
        id: &str,
        error: &str,
        next_run_at_ms: Option<i64>,
        finalize: bool,
    ) -> Result<(), RunsDbError> {
        let conn = self.conn.lock();
        if finalize {
            conn.execute(
                "UPDATE runs SET status='dead', last_error=?1, worker_id=NULL, lease_until=NULL WHERE id = ?2",
                params![error, id],
            )?;
        } else {
            let next = next_run_at_ms.ok_or_else(|| {
                RunsDbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "fail(finalize=false) requires next_run_at_ms",
                    ),
                )))
            })?;
            conn.execute(
                "UPDATE runs SET status='pending', last_error=?1, worker_id=NULL, lease_until=NULL, run_at=?2 WHERE id = ?3",
                params![error, next, id],
            )?;
        }
        Ok(())
    }

    /// Reschedule a run for later without bumping attempts (for durable
    /// `step.sleep` and `step.waitFor` parking). When `wake_at_ms` is None
    /// the run is parked indefinitely (waiting for an event).
    pub fn defer(&self, id: &str, wake_at_ms: Option<i64>) -> Result<(), RunsDbError> {
        let conn = self.conn.lock();
        // i64::MAX = "indefinite" — events.rs will rewrite run_at when a
        // matching signal arrives.
        let run_at = wake_at_ms.unwrap_or(i64::MAX);
        conn.execute(
            "UPDATE runs SET status='pending', worker_id=NULL, lease_until=NULL, run_at=?1, attempts=attempts-1
             WHERE id = ?2",
            params![run_at, id],
        )?;
        Ok(())
    }

    pub fn reclaim_expired(&self) -> Result<u64, RunsDbError> {
        let conn = self.conn.lock();
        let changes = conn.execute(
            "UPDATE runs SET status='pending', worker_id=NULL, lease_until=NULL
             WHERE status='running' AND lease_until IS NOT NULL AND lease_until < ?1",
            params![now_ms()],
        )?;
        Ok(changes as u64)
    }

    /// Park a run waiting for a named event. Stores the waiter and defers
    /// the run. Wake happens via `signal` (or via run_at if a timeout was
    /// set and it elapses).
    pub fn wait_for_event(
        &self,
        run_id: &str,
        step_name: &str,
        event_name: &str,
        timeout_at_ms: Option<i64>,
    ) -> Result<(), RunsDbError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO event_waiters (run_id, step_name, event_name, expires_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id, step_name, event_name, timeout_at_ms],
        )?;
        // Defer: park until timeout or signal arrives. Don't consume retry budget.
        let run_at = timeout_at_ms.unwrap_or(i64::MAX);
        tx.execute(
            "UPDATE runs SET status='pending', worker_id=NULL, lease_until=NULL,
                              run_at=?1, attempts=attempts-1
             WHERE id = ?2",
            params![run_at, run_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Deliver an event payload. Wakes every parked waiter with matching
    /// `event_name`: the payload is stored as the waiter's step result,
    /// the waiter row is removed, and the run is set to pending. Returns
    /// the number of runs woken.
    pub fn signal(
        &self,
        event_name: &str,
        payload: &serde_json::Value,
    ) -> Result<u64, RunsDbError> {
        let payload_json = serde_json::to_string(payload)?;
        let now = now_ms();
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;

        // Materialize the event payload as a step result for every waiter.
        // Then wake the runs and clear the waiter rows.
        let mut stmt =
            tx.prepare("SELECT run_id, step_name FROM event_waiters WHERE event_name = ?1")?;
        let waiters: Vec<(String, String)> = stmt
            .query_map(params![event_name], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut woken = 0u64;
        for (run_id, step_name) in &waiters {
            tx.execute(
                "INSERT OR IGNORE INTO steps (run_id, name, result, completed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![run_id, step_name, payload_json, now],
            )?;
            tx.execute(
                "UPDATE runs SET status='pending', run_at=?1 WHERE id = ?2 AND status='pending'",
                params![now, run_id],
            )?;
            tx.execute(
                "DELETE FROM event_waiters WHERE run_id = ?1 AND step_name = ?2",
                params![run_id, step_name],
            )?;
            woken += 1;
        }
        tx.commit()?;
        Ok(woken)
    }

    pub fn pending_count(&self) -> Result<u64, RunsDbError> {
        let conn = self.conn.lock();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE status='pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> EnqueueOpts {
        EnqueueOpts::default()
    }

    #[test]
    fn enqueue_inserts_a_pending_row() {
        let db = RunsDb::open_in_memory().unwrap();
        let result = db
            .enqueue("send-email", &serde_json::json!({"to":"a@b.c"}), &opts())
            .unwrap();
        assert!(!result.deduplicated);
        assert!(!result.id.is_empty());
        assert_eq!(db.pending_count().unwrap(), 1);
    }

    #[test]
    fn enqueue_deduplicates_on_unique_key() {
        let db = RunsDb::open_in_memory().unwrap();
        let key = Some("cron:5m:0".into());
        let first = db
            .enqueue(
                "w",
                &serde_json::json!({}),
                &EnqueueOpts {
                    unique_key: key.clone(),
                    ..opts()
                },
            )
            .unwrap();
        let second = db
            .enqueue(
                "w",
                &serde_json::json!({}),
                &EnqueueOpts {
                    unique_key: key,
                    ..opts()
                },
            )
            .unwrap();

        assert_eq!(first.id, second.id);
        assert!(!first.deduplicated);
        assert!(second.deduplicated);
        assert_eq!(db.pending_count().unwrap(), 1);
    }

    #[test]
    fn enqueue_different_unique_keys_do_not_collide() {
        let db = RunsDb::open_in_memory().unwrap();
        db.enqueue(
            "w",
            &serde_json::json!({}),
            &EnqueueOpts {
                unique_key: Some("k1".into()),
                ..opts()
            },
        )
        .unwrap();
        db.enqueue(
            "w",
            &serde_json::json!({}),
            &EnqueueOpts {
                unique_key: Some("k2".into()),
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(db.pending_count().unwrap(), 2);
    }

    #[test]
    fn enqueue_without_unique_key_always_inserts() {
        let db = RunsDb::open_in_memory().unwrap();
        db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        assert_eq!(db.pending_count().unwrap(), 2);
    }

    #[test]
    fn enqueue_honors_custom_max_attempts_and_run_at() {
        let db = RunsDb::open_in_memory().unwrap();
        let future = now_ms() + 60_000;
        let r = db
            .enqueue(
                "w",
                &serde_json::json!({}),
                &EnqueueOpts {
                    run_at_ms: Some(future),
                    max_attempts: Some(7),
                    unique_key: None,
                },
            )
            .unwrap();

        let conn = db.conn.lock();
        let (run_at, max_attempts): (i64, i64) = conn
            .query_row(
                "SELECT run_at, max_attempts FROM runs WHERE id = ?1",
                params![r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(run_at, future);
        assert_eq!(max_attempts, 7);
    }

    #[test]
    fn open_creates_parent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("dir").join("runs.db");
        let db = RunsDb::open(&path).unwrap();
        db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn deduplication_frees_slot_once_original_is_terminal() {
        let db = RunsDb::open_in_memory().unwrap();
        let r1 = db
            .enqueue(
                "w",
                &serde_json::json!({}),
                &EnqueueOpts {
                    unique_key: Some("k".into()),
                    ..opts()
                },
            )
            .unwrap();

        {
            let conn = db.conn.lock();
            conn.execute(
                "UPDATE runs SET status='succeeded' WHERE id = ?1",
                params![r1.id],
            )
            .unwrap();
        }

        let r2 = db
            .enqueue(
                "w",
                &serde_json::json!({}),
                &EnqueueOpts {
                    unique_key: Some("k".into()),
                    ..opts()
                },
            )
            .unwrap();
        assert_ne!(r1.id, r2.id);
        assert!(!r2.deduplicated);
    }

    #[test]
    fn save_step_persists_to_steps_table_and_claim_hydrates_state() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        let claimed = db.claim("w1", &["w".into()], 30_000).unwrap().unwrap();
        assert_eq!(claimed.id, r.id);
        assert_eq!(claimed.step_state, serde_json::json!({}));

        db.save_step(&r.id, "fetch-user", &serde_json::json!({"id":"u1"}))
            .unwrap();
        db.save_step(&r.id, "send", &serde_json::json!(true))
            .unwrap();
        // Bounce the run back to pending so we can claim it again.
        db.fail(&r.id, "boom", Some(now_ms()), false).unwrap();

        let claimed2 = db.claim("w2", &["w".into()], 30_000).unwrap().unwrap();
        assert_eq!(claimed2.id, r.id);
        let state = claimed2.step_state.as_object().unwrap();
        assert_eq!(
            state.get("fetch-user"),
            Some(&serde_json::json!({"id":"u1"}))
        );
        assert_eq!(state.get("send"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn save_step_is_idempotent_first_wins() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        db.claim("w1", &["w".into()], 30_000).unwrap();
        db.save_step(&r.id, "fetch", &serde_json::json!("first"))
            .unwrap();
        // Same step name written again — INSERT OR IGNORE keeps the first.
        db.save_step(&r.id, "fetch", &serde_json::json!("second"))
            .unwrap();

        db.fail(&r.id, "x", Some(now_ms()), false).unwrap();
        let claimed = db.claim("w2", &["w".into()], 30_000).unwrap().unwrap();
        assert_eq!(
            claimed.step_state.as_object().unwrap().get("fetch"),
            Some(&serde_json::json!("first"))
        );
    }

    #[test]
    fn complete_marks_succeeded_and_keeps_steps() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        db.claim("w1", &["w".into()], 30_000).unwrap();
        db.save_step(&r.id, "s", &serde_json::json!("v")).unwrap();
        db.complete(&r.id).unwrap();

        let conn = db.conn.lock();
        let status: String = conn
            .query_row(
                "SELECT status FROM runs WHERE id = ?1",
                params![r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "succeeded");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM steps WHERE run_id = ?1",
                params![r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn cancel_marks_cancelled_with_reason() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        db.claim("w1", &["w".into()], 30_000).unwrap();
        db.cancel(&r.id, Some("user cancelled")).unwrap();

        let conn = db.conn.lock();
        let (status, last_error): (String, Option<String>) = conn
            .query_row(
                "SELECT status, last_error FROM runs WHERE id = ?1",
                params![r.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "cancelled");
        assert_eq!(last_error, Some("user cancelled".into()));
    }

    #[test]
    fn defer_sets_run_at_and_decrements_attempts() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        let claimed = db.claim("w1", &["w".into()], 30_000).unwrap().unwrap();
        assert_eq!(claimed.attempts, 1);

        let wake = now_ms() + 60_000;
        db.defer(&r.id, Some(wake)).unwrap();

        let conn = db.conn.lock();
        let (status, run_at, attempts): (String, i64, i64) = conn
            .query_row(
                "SELECT status, run_at, attempts FROM runs WHERE id = ?1",
                params![r.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(run_at, wake);
        // defer rolls attempts back so it doesn't consume retry budget
        assert_eq!(attempts, 0);
    }

    #[test]
    fn defer_with_none_parks_indefinitely() {
        let db = RunsDb::open_in_memory().unwrap();
        let r = db.enqueue("w", &serde_json::json!({}), &opts()).unwrap();
        db.claim("w1", &["w".into()], 30_000).unwrap();
        db.defer(&r.id, None).unwrap();

        let conn = db.conn.lock();
        let run_at: i64 = conn
            .query_row(
                "SELECT run_at FROM runs WHERE id = ?1",
                params![r.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_at, i64::MAX);
    }
}

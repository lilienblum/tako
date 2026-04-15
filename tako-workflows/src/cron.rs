//! Cron ticker + schedule registration.
//!
//! Workers send `Command::RegisterSchedules` on startup; we persist into the
//! `schedules` table. The ticker task wakes every second, walks the schedules,
//! and enqueues any that are due. The unique key
//! `cron:<name>:<bucket_unix_ms>` prevents a single boundary from enqueuing
//! twice even if the ticker runs twice for the same second or the worker
//! re-registers mid-tick.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use cron::Schedule;
use rusqlite::params;
use tako_core::{EnqueueOpts, ScheduleSpec};
use tokio::sync::oneshot;

use super::enqueue::{RunsDb, RunsDbError};

/// Replace the schedules table for this app with the given list.
///
/// Unknown schedules are dropped. Existing schedules keep their `last_run_at`
/// so a re-registration doesn't resurrect already-processed buckets.
pub fn register_schedules(db: &RunsDb, schedules: &[ScheduleSpec]) -> Result<(), RunsDbError> {
    for s in schedules {
        Schedule::from_str(&s.cron).map_err(|e| {
            RunsDbError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("invalid cron '{}' for '{}': {}", s.cron, s.name, e),
                ),
            )))
        })?;
    }

    let mut conn = db.lock_conn();
    let tx = conn.transaction()?;

    let names: Vec<String> = schedules.iter().map(|s| s.name.clone()).collect();
    if names.is_empty() {
        tx.execute("DELETE FROM schedules", [])?;
    } else {
        let placeholders = names.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM schedules WHERE name NOT IN ({})", placeholders);
        let params: Vec<&dyn rusqlite::ToSql> =
            names.iter().map(|n| n as &dyn rusqlite::ToSql).collect();
        tx.execute(&sql, &params[..])?;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    for s in schedules {
        // Set `last_run_at` to now on first insert so subsequent tick_once
        // enqueues the *next* boundary (not the 30 years of boundaries
        // preceding now). On conflict we leave the existing timestamp alone.
        tx.execute(
            "INSERT INTO schedules (name, cron, last_run_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET cron = excluded.cron",
            params![s.name, s.cron, now_ms],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct ScheduleRow {
    name: String,
    cron: String,
    last_run_at: Option<i64>,
}

fn list_schedules(db: &RunsDb) -> Result<Vec<ScheduleRow>, RunsDbError> {
    let conn = db.lock_conn();
    let mut stmt = conn.prepare("SELECT name, cron, last_run_at FROM schedules")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ScheduleRow {
                name: row.get(0)?,
                cron: row.get(1)?,
                last_run_at: row.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn set_last_run_at(db: &RunsDb, name: &str, ts: i64) -> Result<(), RunsDbError> {
    let conn = db.lock_conn();
    conn.execute(
        "UPDATE schedules SET last_run_at = ?1 WHERE name = ?2",
        params![ts, name],
    )?;
    Ok(())
}

/// Fire any schedules whose next boundary is at or before `now_ms`. Returns
/// the number of tasks enqueued.
pub fn tick_once(db: &RunsDb, now_ms: i64) -> Result<u64, RunsDbError> {
    let schedules = list_schedules(db)?;
    let now = DateTime::<Utc>::from_timestamp_millis(now_ms).unwrap_or_else(Utc::now);
    let mut enqueued = 0u64;

    for row in schedules {
        let schedule = match Schedule::from_str(&row.cron) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(name = %row.name, cron = %row.cron, error = %e, "skip invalid cron");
                continue;
            }
        };

        let after = match row.last_run_at {
            Some(ts) => DateTime::<Utc>::from_timestamp_millis(ts).unwrap_or(now),
            None => now - chrono::Duration::seconds(1),
        };
        // Fast-forward: if the server fell behind (idle, sleep, crash), skip
        // intermediate boundaries and enqueue only the latest that has
        // already passed. Prevents a thundering-herd flood when catching up.
        let mut latest: Option<DateTime<Utc>> = None;
        for t in schedule.after(&after) {
            if t > now {
                break;
            }
            latest = Some(t);
        }
        let Some(next) = latest else {
            continue;
        };

        let bucket_ms = next.timestamp_millis();
        let unique_key = format!("cron:{}:{}", row.name, bucket_ms);
        let opts = EnqueueOpts {
            unique_key: Some(unique_key),
            run_at_ms: Some(bucket_ms),
            max_attempts: None,
        };
        db.enqueue(&row.name, &serde_json::json!({}), &opts)?;
        set_last_run_at(db, &row.name, bucket_ms)?;
        enqueued += 1;
    }

    Ok(enqueued)
}

/// Handle to the running ticker task. Drop stops the loop.
pub struct CronTickerHandle {
    shutdown_tx: Option<oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl CronTickerHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }
}

impl Drop for CronTickerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Start a cron ticker for an app. Optional `on_enqueue` is called whenever
/// a tick enqueues at least one task — the supervisor wires this to `wake()`
/// so scale-to-zero workers spin up.
pub fn spawn(db: Arc<RunsDb>, on_enqueue: Arc<dyn Fn() + Send + Sync>) -> CronTickerHandle {
    let (tx, mut rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut rx => break,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    match tick_once(&db, now_ms) {
                        Ok(n) if n > 0 => (on_enqueue)(),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "cron tick failed"),
                    }
                }
            }
        }
    });
    CronTickerHandle {
        shutdown_tx: Some(tx),
        join: Some(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Arc<RunsDb> {
        Arc::new(RunsDb::open_in_memory().unwrap())
    }

    #[test]
    fn register_schedules_inserts_rows() {
        let db = db();
        register_schedules(
            &db,
            &[
                ScheduleSpec {
                    name: "a".into(),
                    cron: "0 */5 * * * *".into(),
                },
                ScheduleSpec {
                    name: "b".into(),
                    cron: "0 0 * * * *".into(),
                },
            ],
        )
        .unwrap();

        let schedules = list_schedules(&db).unwrap();
        assert_eq!(schedules.len(), 2);
    }

    #[test]
    fn register_schedules_is_idempotent_on_repeat_call() {
        let db = db();
        let s = ScheduleSpec {
            name: "a".into(),
            cron: "0 */5 * * * *".into(),
        };
        register_schedules(&db, std::slice::from_ref(&s)).unwrap();
        register_schedules(&db, std::slice::from_ref(&s)).unwrap();
        assert_eq!(list_schedules(&db).unwrap().len(), 1);
    }

    #[test]
    fn register_schedules_removes_schedules_not_in_new_list() {
        let db = db();
        register_schedules(
            &db,
            &[
                ScheduleSpec {
                    name: "keep".into(),
                    cron: "0 */5 * * * *".into(),
                },
                ScheduleSpec {
                    name: "drop".into(),
                    cron: "0 0 * * * *".into(),
                },
            ],
        )
        .unwrap();
        register_schedules(
            &db,
            &[ScheduleSpec {
                name: "keep".into(),
                cron: "0 */5 * * * *".into(),
            }],
        )
        .unwrap();
        let rows = list_schedules(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "keep");
    }

    #[test]
    fn register_schedules_rejects_invalid_cron() {
        let db = db();
        let err = register_schedules(
            &db,
            &[ScheduleSpec {
                name: "a".into(),
                cron: "not a cron".into(),
            }],
        )
        .unwrap_err();
        assert!(format!("{err}").to_lowercase().contains("invalid"));
    }

    #[test]
    fn tick_enqueues_due_schedules() {
        let db = db();
        register_schedules(
            &db,
            &[ScheduleSpec {
                name: "every-sec".into(),
                cron: "* * * * * *".into(),
            }],
        )
        .unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis() + 60_000;
        let count = tick_once(&db, now_ms).unwrap();
        assert_eq!(count, 1);
        assert_eq!(db.pending_count().unwrap(), 1);
    }

    #[test]
    fn tick_is_idempotent_within_same_bucket() {
        let db = db();
        register_schedules(
            &db,
            &[ScheduleSpec {
                name: "min".into(),
                cron: "0 * * * * *".into(), // every minute boundary
            }],
        )
        .unwrap();

        let now_ms = chrono::Utc::now().timestamp_millis() + 120_000;
        let first = tick_once(&db, now_ms).unwrap();
        let second = tick_once(&db, now_ms).unwrap();
        assert!(first >= 1);
        // Second tick shouldn't enqueue again for the same bucket — the
        // unique_key dedup catches it and last_run_at was advanced.
        assert_eq!(second, 0);
    }
}

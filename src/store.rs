//! One SQLite file for job history. The Docker daemon remains the source of
//! truth for how many slots are taken.

use crate::{REASON_START_FAILED, REASON_TIMEOUT};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    pub id: String,
    pub image: String,
    pub job_key: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i64>,
    pub reason: Option<String>,
    pub callback_url: Option<String>,
    pub logs: Option<String>,
}

impl JobRecord {
    pub fn is_finished(&self) -> bool {
        self.ended_at.is_some()
    }

    pub fn status(&self) -> &'static str {
        if self.is_finished() {
            "exited"
        } else {
            "running"
        }
    }

    pub fn duration_secs(&self) -> Option<i64> {
        self.ended_at
            .map(|end| end.saturating_sub(self.started_at).max(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobCounts {
    pub started: u64,
    pub finished: u64,
    pub timeouts: u64,
}

#[derive(Debug)]
pub struct NewJob {
    pub id: String,
    pub image: String,
    pub job_key: Option<String>,
    pub started_at: i64,
    pub callback_url: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Sql(String),
    #[error("job_key already exists")]
    Conflict,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| StoreError::Sql(err.to_string()))?;
            }
        }
        let conn = Connection::open(path).map_err(sql)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                image TEXT NOT NULL,
                job_key TEXT,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                exit_code INTEGER,
                reason TEXT,
                callback_url TEXT,
                logs TEXT
            );
            CREATE UNIQUE INDEX IF NOT EXISTS jobs_job_key
                ON jobs(job_key) WHERE job_key IS NOT NULL;
            CREATE INDEX IF NOT EXISTS jobs_ended_at ON jobs(ended_at);
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS auth_failures (
                ip TEXT PRIMARY KEY,
                failures INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS bans (
                ip TEXT PRIMARY KEY,
                banned_until INTEGER NOT NULL
            );
            ",
        )
        .map_err(sql)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row("SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs FROM jobs WHERE id = ?1", [id], map_job)
            .optional()
            .map_err(sql)
    }

    /// The job that started last, for the local status page.
    pub fn last_job(&self) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs FROM jobs ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    pub fn get_by_key(&self, key: &str) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs FROM jobs WHERE job_key = ?1",
            [key],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    pub fn insert_running(&self, job: &NewJob) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let result = conn.execute(
            "INSERT INTO jobs (id, image, job_key, started_at, callback_url) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job.id, job.image, job.job_key, job.started_at, job.callback_url],
        );
        match result {
            Ok(_) => Ok(()),
            Err(err) if is_constraint(&err) && err.to_string().contains("job_key") => {
                Err(StoreError::Conflict)
            }
            Err(err) => Err(sql(err)),
        }
    }

    /// A start that never produced a running container. Holds `job_key` so a retry
    /// sees this row instead of launching again.
    pub fn insert_terminal(
        &self,
        job: &NewJob,
        reason: &str,
        exit_code: Option<i64>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let result = conn.execute(
            "INSERT INTO jobs (id, image, job_key, started_at, ended_at, exit_code, reason, callback_url)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)",
            params![job.id, job.image, job.job_key, job.started_at, exit_code, reason, job.callback_url],
        );
        match result {
            Ok(_) => Ok(()),
            Err(err) if is_constraint(&err) => Err(StoreError::Conflict),
            Err(err) => Err(sql(err)),
        }
    }

    /// Returns the row only when this call is the one that finished it.
    pub fn finish(
        &self,
        id: &str,
        ended_at: i64,
        exit_code: Option<i64>,
        reason: &str,
        logs: &str,
    ) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let changed = conn
            .execute(
                "UPDATE jobs SET ended_at = ?1, exit_code = ?2, reason = ?3, logs = ?4
                 WHERE id = ?5 AND ended_at IS NULL",
                params![ended_at, exit_code, reason, logs, id],
            )
            .map_err(sql)?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs FROM jobs WHERE id = ?1",
            [id],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    pub fn running_ids(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT id FROM jobs WHERE ended_at IS NULL")
            .map_err(sql)?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .map_err(sql)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }

    /// Finished rows whose end time is strictly before `cutoff_unix`. Running rows stay.
    pub fn purge_finished_before(&self, cutoff_unix: i64) -> Result<usize, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let n = conn
            .execute(
                "DELETE FROM jobs WHERE ended_at IS NOT NULL AND ended_at < ?1",
                [cutoff_unix],
            )
            .map_err(sql)?;
        Ok(n)
    }

    /// The jobs that started last, newest first.
    pub fn recent(&self, limit: usize) -> Result<Vec<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(
                "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs
                 FROM jobs ORDER BY started_at DESC, rowid DESC LIMIT ?1",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map([limit as i64], map_job)
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }

    /// The ceiling of the history: finished rows beyond the newest `keep`
    /// go, whatever `history_days` says. Running rows stay.
    pub fn purge_beyond(&self, keep: usize) -> Result<usize, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "DELETE FROM jobs WHERE ended_at IS NOT NULL AND id NOT IN (
                 SELECT id FROM jobs WHERE ended_at IS NOT NULL
                 ORDER BY started_at DESC, rowid DESC LIMIT ?1)",
            [keep as i64],
        )
        .map_err(sql)
    }

    pub fn counts(&self) -> Result<JobCounts, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let started: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
            .map_err(sql)?;
        let finished: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE ended_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(sql)?;
        let timeouts: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE reason = ?1",
                [REASON_TIMEOUT],
                |row| row.get(0),
            )
            .map_err(sql)?;
        Ok(JobCounts {
            started: started.max(0) as u64,
            finished: finished.max(0) as u64,
            timeouts: timeouts.max(0) as u64,
        })
    }

    pub fn images(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT DISTINCT image FROM jobs WHERE reason IS NULL OR reason != ?1")
            .map_err(sql)?;
        let rows = stmt
            .query_map([REASON_START_FAILED], |row| row.get(0))
            .map_err(sql)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }

    pub fn draining(&self) -> Result<bool, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let value: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'draining'", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sql)?;
        Ok(value.as_deref() == Some("1"))
    }

    pub fn set_draining(&self, draining: bool) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('draining', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [if draining { "1" } else { "0" }],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// `Some(until)` when `ip` is banned at `now`. Expired rows are deleted.
    pub fn active_ban(&self, ip: &str, now: i64) -> Result<Option<i64>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let until: Option<i64> = conn
            .query_row("SELECT banned_until FROM bans WHERE ip = ?1", [ip], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sql)?;
        match until {
            Some(until) if until > now => Ok(Some(until)),
            Some(_) => {
                conn.execute("DELETE FROM bans WHERE ip = ?1", [ip])
                    .map_err(sql)?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Count one failed authentication. At `limit` the IP is banned and the counter resets.
    pub fn record_failure(
        &self,
        ip: &str,
        now: i64,
        limit: u32,
        ban_secs: i64,
    ) -> Result<Strike, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let current: i64 = conn
            .query_row(
                "SELECT failures FROM auth_failures WHERE ip = ?1",
                [ip],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?
            .unwrap_or(0);
        let next = current + 1;
        if next >= i64::from(limit) {
            let until = now.saturating_add(ban_secs);
            conn.execute(
                "INSERT INTO bans (ip, banned_until) VALUES (?1, ?2)
                 ON CONFLICT(ip) DO UPDATE SET banned_until = excluded.banned_until",
                params![ip, until],
            )
            .map_err(sql)?;
            conn.execute("DELETE FROM auth_failures WHERE ip = ?1", [ip])
                .map_err(sql)?;
            return Ok(Strike::Banned { until });
        }
        conn.execute(
            "INSERT INTO auth_failures (ip, failures) VALUES (?1, ?2)
             ON CONFLICT(ip) DO UPDATE SET failures = excluded.failures",
            params![ip, next],
        )
        .map_err(sql)?;
        Ok(Strike::Failed { count: next as u32 })
    }

    pub fn clear_failures(&self, ip: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute("DELETE FROM auth_failures WHERE ip = ?1", [ip])
            .map_err(sql)?;
        Ok(())
    }

    pub fn list_bans(&self, now: i64) -> Result<Vec<Ban>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute("DELETE FROM bans WHERE banned_until <= ?1", [now])
            .map_err(sql)?;
        let mut stmt = conn
            .prepare("SELECT ip, banned_until FROM bans ORDER BY ip")
            .map_err(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Ban {
                    ip: row.get(0)?,
                    banned_until: row.get(1)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }

    pub fn lift_ban(&self, ip: &str) -> Result<bool, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let removed = conn
            .execute("DELETE FROM bans WHERE ip = ?1", [ip])
            .map_err(sql)?;
        conn.execute("DELETE FROM auth_failures WHERE ip = ?1", [ip])
            .map_err(sql)?;
        Ok(removed > 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ban {
    pub ip: String,
    pub banned_until: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strike {
    Failed { count: u32 },
    Banned { until: i64 },
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: row.get(0)?,
        image: row.get(1)?,
        job_key: row.get(2)?,
        started_at: row.get(3)?,
        ended_at: row.get(4)?,
        exit_code: row.get(5)?,
        reason: row.get(6)?,
        callback_url: row.get(7)?,
        logs: row.get(8)?,
    })
}

fn sql(err: rusqlite::Error) -> StoreError {
    StoreError::Sql(err.to_string())
}

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{REASON_EXITED, REASON_LAUNCHER, REASON_START_FAILED};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_store() -> Store {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("shutup-and-run-store-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Store::open(&path.join("jobs.sqlite")).unwrap()
    }

    fn job(id: &str, key: Option<&str>, started: i64) -> NewJob {
        NewJob {
            id: id.into(),
            image: "alpine".into(),
            job_key: key.map(str::to_string),
            started_at: started,
            callback_url: None,
        }
    }

    #[test]
    fn purge_drops_old_finished_rows_and_keeps_running() {
        let store = temp_store();
        let now = 1_700_000_000;
        store
            .insert_running(&job("aa", None, now - 40 * 86_400))
            .unwrap();
        store
            .finish("aa", now - 31 * 86_400, Some(0), REASON_EXITED, "")
            .unwrap();
        store
            .insert_running(&job("bb", None, now - 2 * 86_400))
            .unwrap();
        store
            .finish("bb", now - 86_400, Some(0), REASON_EXITED, "")
            .unwrap();
        store
            .insert_running(&job("cc", Some("live"), now - 100 * 86_400))
            .unwrap();

        let removed = store.purge_finished_before(now - 30 * 86_400).unwrap();
        assert_eq!(removed, 1);
        assert!(store.get("aa").unwrap().is_none());
        assert!(store.get("bb").unwrap().unwrap().is_finished());
        assert!(!store.get("cc").unwrap().unwrap().is_finished());
    }

    #[test]
    fn job_key_conflict_and_reasons() {
        let store = temp_store();
        store
            .insert_running(&job("aa11", Some("same"), 10))
            .unwrap();
        assert!(matches!(
            store.insert_running(&job("bb22", Some("same"), 11)),
            Err(StoreError::Conflict)
        ));
        let again = store.get_by_key("same").unwrap().unwrap();
        assert_eq!(again.id, "aa11");

        store
            .finish("aa11", 20, Some(137), REASON_LAUNCHER, "bye")
            .unwrap();
        let row = store.get("aa11").unwrap().unwrap();
        assert_eq!(row.reason.as_deref(), Some(REASON_LAUNCHER));
        assert_eq!(row.logs.as_deref(), Some("bye"));

        store
            .insert_terminal(&job("cc33", Some("bad"), 30), REASON_START_FAILED, None)
            .unwrap();
        assert_eq!(
            store.get_by_key("bad").unwrap().unwrap().reason.as_deref(),
            Some(REASON_START_FAILED)
        );
    }

    #[test]
    fn drain_flag_survives_reopen() {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("shutup-and-run-drain-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("jobs.sqlite");
        {
            let store = Store::open(&path).unwrap();
            assert!(!store.draining().unwrap());
            store.set_draining(true).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.draining().unwrap());
    }
}

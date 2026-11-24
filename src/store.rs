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
    /// Docker confirmed the kernel killed a process of this container for
    /// memory. False for every other end, a plain SIGKILL included.
    pub oom_killed: bool,
    /// CPU time the job used, in milliseconds, and the most memory it held,
    /// in bytes: from readings every 30 seconds while it ran, so the last
    /// seconds and a short peak can be missed. None: no reading (a job that
    /// ended within its first seconds, or a history from before 0.2.19).
    pub cpu_ms: Option<u64>,
    pub memory_peak: Option<u64>,
    /// Bytes the job received and sent on the network, from the same
    /// readings. Not the pull of its image. None: not read (also before 0.2.23).
    pub network_bytes: Option<u64>,
    /// Bytes the job wrote inside its container (its layer, `SizeRw`), read
    /// as the container is removed. None: not read (also before 0.2.24).
    pub disk_bytes: Option<u64>,
    /// While its image downloads: the bytes so far and the size of the
    /// layers Docker has named until then, which grows as it names more.
    /// None once the pull is over (and before 0.2.25).
    pub pull: Option<(u64, u64)>,
}

/// Something that happened to the machine or to a job's resources, kept for
/// the owner and for a report: what, when, and the numbers. Never a job's
/// env, command or output.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Incident {
    pub at: i64,
    pub kind: String,
    pub detail: String,
}

/// The most incidents kept. Older ones go when a new one is written.
pub const MAX_INCIDENTS: usize = 200;

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

/// Counts over the rows the history still holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Delivery {
    /// Jobs that ended with reason `interrupted`.
    pub interrupted: u64,
    /// Of those, the ones whose callback was not answered (yet, or ever).
    pub interrupted_unnotified: u64,
    /// Callbacks still being retried.
    pub pending: u64,
    /// Callbacks the worker gave up on.
    pub failed: u64,
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
    /// The managed container definition this run belongs to. None for a
    /// pushed or polled job.
    pub managed: Option<String>,
    /// The memory limit its container gets, in bytes. None: no limit.
    pub memory_limit: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Sql(String),
    #[error("job_key already exists")]
    Conflict,
    /// The machine is in the other mode: a managed run while it takes jobs,
    /// or a job while it runs managed containers. Nothing was written.
    #[error("the machine is in the other mode")]
    Mode,
}

/// One managed container definition and what the worker knows about it.
/// `spec` is the definition as JSON, without secret values; `secrets` is
/// the JSON object of those values and leaves this file only as a
/// container's env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedRow {
    pub name: String,
    pub spec: String,
    pub secrets: String,
    pub enabled: bool,
    /// Stopped by a person: nothing starts it but a person.
    pub held: bool,
    pub start_requested: bool,
    pub stop_requested: bool,
    /// Runs in a row that did not last.
    pub failures: i64,
    /// The last run whose end was counted.
    pub accounted_job: Option<String>,
    pub last_job: Option<String>,
    pub last_started_at: Option<i64>,
    /// No automatic start before this.
    pub next_attempt_at: i64,
    /// Why the last start did not happen. Words of this program only.
    pub note: Option<String>,
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
            CREATE TABLE IF NOT EXISTS incidents (
                at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                detail TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS managed (
                name TEXT PRIMARY KEY,
                spec TEXT NOT NULL,
                secrets TEXT NOT NULL DEFAULT '{}',
                enabled INTEGER NOT NULL DEFAULT 0,
                held INTEGER NOT NULL DEFAULT 0,
                start_requested INTEGER NOT NULL DEFAULT 0,
                stop_requested INTEGER NOT NULL DEFAULT 0,
                failures INTEGER NOT NULL DEFAULT 0,
                accounted_job TEXT,
                last_job TEXT,
                last_started_at INTEGER,
                next_attempt_at INTEGER NOT NULL DEFAULT 0,
                note TEXT
            );
            CREATE TABLE IF NOT EXISTS memory_hours (
                slot INTEGER PRIMARY KEY,
                at INTEGER NOT NULL,
                lowest INTEGER NOT NULL,
                total INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS memory_waits (
                day INTEGER PRIMARY KEY,
                secs INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS images (
                image TEXT PRIMARY KEY,
                last_used INTEGER NOT NULL
            );
            ",
        )
        .map_err(sql)?;
        // The images of a history older than 0.2.22 are known from its jobs;
        // an image whose jobs were already purged is not, and stays.
        conn.execute(
            "INSERT OR IGNORE INTO images (image, last_used)
             SELECT image, MAX(started_at) FROM jobs GROUP BY image",
            [],
        )
        .map_err(sql)?;
        // A history written before 0.2.11 has no such column. Its rows keep
        // NULL: their callbacks were sent, or not, by the old three tries.
        // The same for `oom_killed`, added in 0.2.13: NULL reads as false,
        // and for `managed`: NULL is a pushed or polled job. `cpu_ms` and
        // `memory_peak`, 0.2.19, `network_bytes`, 0.2.23, and `disk_bytes`,
        // 0.2.24: NULL is "not measured". `pull_done` and `pull_total`,
        // 0.2.25, are NULL but while an image downloads. `memory_limit`,
        // 0.2.33: NULL is no limit, or a row older than the column.
        for column in [
            "callback_state TEXT",
            "oom_killed INTEGER",
            "managed TEXT",
            "cpu_ms INTEGER",
            "memory_peak INTEGER",
            "network_bytes INTEGER",
            "disk_bytes INTEGER",
            "pull_done INTEGER",
            "pull_total INTEGER",
            "memory_limit INTEGER",
        ] {
            match conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {column}"), []) {
                Ok(_) => {}
                Err(err) if err.to_string().contains("duplicate column") => {}
                Err(err) => return Err(sql(err)),
            }
        }
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
        conn.query_row("SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total FROM jobs WHERE id = ?1", [id], map_job)
            .optional()
            .map_err(sql)
    }

    /// The job that started last, for the local status page.
    pub fn last_job(&self) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total FROM jobs ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    pub fn get_by_key(&self, key: &str) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total FROM jobs WHERE job_key = ?1",
            [key],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    /// One statement, so the mode cannot change between the check and the
    /// row, whichever process changes it: a run of a managed container is
    /// written only in managed mode, and any other job only outside it.
    pub fn insert_running(&self, job: &NewJob) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mode = if job.managed.is_some() {
            "managed"
        } else {
            "jobs"
        };
        let result = conn.execute(
            "INSERT INTO jobs (id, image, job_key, started_at, callback_url, managed, memory_limit)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?8
             WHERE COALESCE((SELECT value FROM meta WHERE key = 'mode'), 'jobs') = ?7",
            params![
                job.id,
                job.image,
                job.job_key,
                job.started_at,
                job.callback_url,
                job.managed,
                mode,
                job.memory_limit
            ],
        );
        match result {
            Ok(0) => Err(StoreError::Mode),
            Ok(_) => note_image(&conn, &job.image, job.started_at),
            Err(err) if is_constraint(&err) && err.to_string().contains("job_key") => {
                Err(StoreError::Conflict)
            }
            Err(err) => Err(sql(err)),
        }
    }

    /// The memory limits of the jobs that have not ended, added up: what
    /// they may come to use together, whatever they use now.
    pub fn committed_memory(&self) -> Result<u64, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT COALESCE(SUM(MAX(memory_limit, 0)), 0) FROM jobs WHERE ended_at IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|sum| sum.max(0) as u64)
        .map_err(sql)
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
                "UPDATE jobs SET ended_at = ?1, exit_code = ?2, reason = ?3, logs = ?4,
                 callback_state = CASE WHEN callback_url IS NOT NULL THEN 'pending' END
                 WHERE id = ?5 AND ended_at IS NULL",
                params![ended_at, exit_code, reason, logs, id],
            )
            .map_err(sql)?;
        if changed == 0 {
            return Ok(None);
        }
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total FROM jobs WHERE id = ?1",
            [id],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    /// `delivered` once the callback was answered with 2xx, `failed` when
    /// the worker gave up. Only a row still `pending` changes.
    pub fn set_callback_state(&self, id: &str, state: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "UPDATE jobs SET callback_state = ?1 WHERE id = ?2 AND callback_state = 'pending'",
            params![state, id],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// Callbacks not answered yet, of jobs that ended in `[from, until]`.
    pub fn pending_callbacks(&self, from: i64, until: i64) -> Result<Vec<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(
                "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total
                 FROM jobs WHERE callback_state = 'pending' AND ended_at BETWEEN ?1 AND ?2",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map([from, until], map_job)
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(rows)
    }

    /// Gives up on callbacks of jobs that ended before `cutoff`.
    pub fn expire_callbacks(&self, cutoff: i64) -> Result<usize, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "UPDATE jobs SET callback_state = 'failed'
             WHERE callback_state = 'pending' AND ended_at < ?1",
            [cutoff],
        )
        .map_err(sql)
    }

    /// For the status page: interrupted jobs in the history, how many of
    /// them nobody was told about, and callbacks waiting and given up.
    pub fn delivery(&self) -> Result<Delivery, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let count = |sql_text: &str| -> Result<u64, StoreError> {
            conn.query_row(sql_text, [], |row| row.get::<_, i64>(0))
                .map(|n| n.max(0) as u64)
                .map_err(sql)
        };
        Ok(Delivery {
            interrupted: count("SELECT COUNT(*) FROM jobs WHERE reason = 'interrupted'")?,
            interrupted_unnotified: count(
                "SELECT COUNT(*) FROM jobs WHERE reason = 'interrupted'
                 AND callback_state IN ('pending', 'failed')",
            )?,
            pending: count("SELECT COUNT(*) FROM jobs WHERE callback_state = 'pending'")?,
            failed: count("SELECT COUNT(*) FROM jobs WHERE callback_state = 'failed'")?,
        })
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

    /// The jobs whose image is downloading: id, bytes so far, size known so far.
    pub fn pulling(&self) -> Result<Vec<(String, u64, u64)>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(
                "SELECT id, pull_done, pull_total FROM jobs \
                 WHERE ended_at IS NULL AND pull_total IS NOT NULL ORDER BY id",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
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
                "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total
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

    /// Marks a job as killed for memory. Before `finish`, so the row the
    /// callback is built from already says so.
    pub fn mark_oom(&self, id: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute("UPDATE jobs SET oom_killed = 1 WHERE id = ?1", [id])
            .map_err(sql)?;
        Ok(())
    }

    /// What a job used, from its readings. Before `finish` or after it.
    pub fn set_usage(
        &self,
        id: &str,
        cpu_ms: Option<u64>,
        memory_peak: Option<u64>,
        network_bytes: Option<u64>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let big = |v: Option<u64>| v.map(|v| v.min(i64::MAX as u64) as i64);
        conn.execute(
            "UPDATE jobs SET cpu_ms = ?2, memory_peak = ?3, network_bytes = ?4 WHERE id = ?1",
            params![id, big(cpu_ms), big(memory_peak), big(network_bytes)],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// What a job wrote to its container's disk, read once at its end.
    pub fn set_disk(&self, id: &str, bytes: u64) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "UPDATE jobs SET disk_bytes = ?2 WHERE id = ?1",
            params![id, bytes.min(i64::MAX as u64) as i64],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// How far a job's image download is; None when it is over.
    pub fn set_pull(&self, id: &str, pull: Option<(u64, u64)>) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let big = |v: u64| v.min(i64::MAX as u64) as i64;
        conn.execute(
            "UPDATE jobs SET pull_done = ?2, pull_total = ?3 WHERE id = ?1",
            params![id, pull.map(|p| big(p.0)), pull.map(|p| big(p.1))],
        )
        .map_err(sql)?;
        Ok(())
    }

    pub fn add_incident(&self, at: i64, kind: &str, detail: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO incidents (at, kind, detail) VALUES (?1, ?2, ?3)",
            params![at, kind, detail],
        )
        .map_err(sql)?;
        conn.execute(
            "DELETE FROM incidents WHERE rowid NOT IN
             (SELECT rowid FROM incidents ORDER BY rowid DESC LIMIT ?1)",
            [MAX_INCIDENTS as i64],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// The owner's memory in this hour of the week (`slot`, Monday 00:00 is
    /// 0): its lowest during the current pass of that hour. A row written
    /// more than two hours ago is the hour's previous pass, a week back,
    /// and is replaced.
    pub fn note_memory_hour(
        &self,
        slot: u32,
        at: i64,
        available: u64,
        total: u64,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO memory_hours (slot, at, lowest, total) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(slot) DO UPDATE SET
               lowest = CASE WHEN memory_hours.at >= ?2 - 7200
                        THEN MIN(memory_hours.lowest, ?3) ELSE ?3 END,
               total = ?4, at = ?2",
            params![slot, at, available as i64, total as i64],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// Every hour of the week with a reading since `since`.
    pub fn memory_hours(&self, since: i64) -> Result<Vec<crate::memory::Hour>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT slot, lowest, total FROM memory_hours WHERE at >= ?1")
            .map_err(sql)?;
        let rows = stmt
            .query_map([since], |row| {
                Ok(crate::memory::Hour {
                    slot: row.get::<_, i64>(0)? as u32,
                    lowest: row.get::<_, i64>(1)? as u64,
                    total: row.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// Adds seconds a new job waited for memory to `day` (days since 1970
    /// on the PC's clock). Days older than 30 are dropped.
    pub fn add_memory_wait(&self, day: i64, secs: u64) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO memory_waits (day, secs) VALUES (?1, ?2)
             ON CONFLICT(day) DO UPDATE SET secs = memory_waits.secs + ?2",
            params![day, secs as i64],
        )
        .map_err(sql)?;
        conn.execute("DELETE FROM memory_waits WHERE day < ?1", [day - 30])
            .map_err(sql)?;
        Ok(())
    }

    /// Seconds waited for memory on `day`, and over the seven days to it.
    pub fn memory_waited(&self, day: i64) -> Result<(u64, u64), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN day = ?1 THEN secs END), 0),
                    COALESCE(SUM(secs), 0)
             FROM memory_waits WHERE day > ?1 - 7 AND day <= ?1",
            [day],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )
        .map_err(sql)
    }

    /// The newest incidents first.
    pub fn incidents(&self, limit: usize) -> Result<Vec<Incident>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT at, kind, detail FROM incidents ORDER BY rowid DESC LIMIT ?1")
            .map_err(sql)?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                Ok(Incident {
                    at: row.get(0)?,
                    kind: row.get(1)?,
                    detail: row.get(2)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    /// When the jobs killed for memory since `since` ended.
    /// The images whose last job started before `cutoff`: unused since.
    pub fn images_unused_since(&self, cutoff: i64) -> Result<Vec<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT image FROM images WHERE last_used < ?1 ORDER BY image")
            .map_err(sql)?;
        let rows = stmt.query_map([cutoff], |row| row.get(0)).map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    /// An image that was removed: a later job with it is a new first use.
    pub fn forget_image(&self, image: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute("DELETE FROM images WHERE image = ?1", [image])
            .map_err(sql)?;
        Ok(())
    }

    /// The last pass that removed what was unused: when, how many images,
    /// and the disk it gave back.
    pub fn cleanup(&self) -> Result<Option<Cleanup>, StoreError> {
        Ok(self
            .meta("cleanup")?
            .and_then(|text| serde_json::from_str(&text).ok()))
    }

    pub fn set_cleanup(&self, cleanup: &Cleanup) -> Result<(), StoreError> {
        let text = serde_json::to_string(cleanup).map_err(|e| StoreError::Sql(e.to_string()))?;
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('cleanup', ?1)
             ON CONFLICT(key) DO UPDATE SET value = ?1",
            [text],
        )
        .map_err(sql)?;
        Ok(())
    }

    pub fn oom_times(&self, since: i64) -> Result<Vec<i64>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT at FROM incidents WHERE kind = 'oom' AND at >= ?1")
            .map_err(sql)?;
        let rows = stmt.query_map([since], |row| row.get(0)).map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)
    }

    /// The owner's authorization to run under a warning, when one was given
    /// and not withdrawn. A value that does not parse is no authorization.
    pub fn safety_consent(&self) -> Result<Option<crate::safety::Consent>, StoreError> {
        Ok(self
            .meta("safety_consent")?
            .and_then(|text| serde_json::from_str(&text).ok()))
    }

    /// Writes or, with None, withdraws the authorization.
    pub fn set_safety_consent(
        &self,
        consent: Option<&crate::safety::Consent>,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        match consent {
            Some(consent) => conn.execute(
                "INSERT INTO meta (key, value) VALUES ('safety_consent', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [serde_json::to_string(consent).unwrap_or_default()],
            ),
            None => conn.execute("DELETE FROM meta WHERE key = 'safety_consent'", []),
        }
        .map_err(sql)?;
        Ok(())
    }

    /// True while the machine runs managed containers and takes no job.
    pub fn managed_mode(&self) -> Result<bool, StoreError> {
        Ok(self.meta("mode")?.as_deref() == Some("managed"))
    }

    /// Changes the mode only while no job and no managed run is unfinished,
    /// in one statement. `Ok(false)`: the worker is busy and nothing changed.
    pub fn set_managed_mode(&self, managed: bool) -> Result<bool, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let changed = conn
            .execute(
                "INSERT INTO meta (key, value)
                 SELECT 'mode', ?1 WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE ended_at IS NULL)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [if managed { "managed" } else { "jobs" }],
            )
            .map_err(sql)?;
        Ok(changed > 0)
    }

    pub fn managed_rows(&self) -> Result<Vec<ManagedRow>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {MANAGED_COLUMNS} FROM managed ORDER BY name"
            ))
            .map_err(sql)?;
        let rows = stmt.query_map([], map_managed).map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    pub fn managed_row(&self, name: &str) -> Result<Option<ManagedRow>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            &format!("SELECT {MANAGED_COLUMNS} FROM managed WHERE name = ?1"),
            [name],
            map_managed,
        )
        .optional()
        .map_err(sql)
    }

    /// Writes the definition. A new one starts with no history; an edited
    /// one keeps its secrets unless given, and gets a fresh count: what
    /// failed before may be what the edit fixed.
    pub fn save_managed(
        &self,
        name: &str,
        spec: &str,
        secrets: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO managed (name, spec, secrets, enabled) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET spec = excluded.spec, secrets = excluded.secrets,
                 enabled = excluded.enabled, failures = 0, next_attempt_at = 0, note = NULL,
                 start_requested = CASE WHEN excluded.enabled = 1 THEN start_requested ELSE 0 END",
            params![name, spec, secrets, enabled],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// Removes a definition that has no unfinished run. `Ok(false)`: it
    /// runs, or there is no such definition.
    pub fn delete_managed(&self, name: &str) -> Result<bool, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let removed = conn
            .execute(
                "DELETE FROM managed WHERE name = ?1 AND NOT EXISTS
                 (SELECT 1 FROM jobs WHERE managed = ?1 AND ended_at IS NULL)",
                [name],
            )
            .map_err(sql)?;
        Ok(removed > 0)
    }

    /// Some columns of a definition's state. `assignments` is this
    /// program's own text, never input.
    fn set_managed(
        &self,
        name: &str,
        assignments: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<bool, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        let mut all: Vec<&dyn rusqlite::ToSql> = values.to_vec();
        all.push(&name);
        let changed = conn
            .execute(
                &format!(
                    "UPDATE managed SET {assignments} WHERE name = ?{}",
                    all.len()
                ),
                all.as_slice(),
            )
            .map_err(sql)?;
        Ok(changed > 0)
    }

    /// Enabling is a fresh start: the hold and the failure count go.
    /// Disabling drops a start that was asked for and not made.
    pub fn set_managed_enabled(&self, name: &str, enabled: bool) -> Result<bool, StoreError> {
        if enabled {
            self.set_managed(
                name,
                "enabled = 1, held = 0, failures = 0, next_attempt_at = 0, note = NULL",
                &[],
            )
        } else {
            self.set_managed(name, "enabled = 0, start_requested = 0", &[])
        }
    }

    /// A person asked for a start: the hold and the backoff go.
    pub fn request_managed_start(&self, name: &str) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "start_requested = 1, stop_requested = 0, held = 0, failures = 0, next_attempt_at = 0",
            &[],
        )
    }

    /// A person asked for a stop: held from now, whether or not it runs.
    pub fn request_managed_stop(&self, name: &str) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "stop_requested = 1, start_requested = 0, held = 1",
            &[],
        )
    }

    pub fn clear_managed_stop(&self, name: &str) -> Result<bool, StoreError> {
        self.set_managed(name, "stop_requested = 0", &[])
    }

    pub fn clear_managed_start(&self, name: &str) -> Result<bool, StoreError> {
        self.set_managed(name, "start_requested = 0", &[])
    }

    pub fn set_managed_secrets(&self, name: &str, secrets: &str) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "secrets = ?1, next_attempt_at = 0, note = NULL",
            &[&secrets],
        )
    }

    /// A run was started.
    pub fn note_managed_started(&self, name: &str, job: &str, at: i64) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "last_job = ?1, last_started_at = ?2, start_requested = 0, held = 0, note = NULL",
            &[&job, &at],
        )
    }

    /// A run's end was counted: the failures in a row and when the next
    /// automatic start may be.
    pub fn note_managed_ended(
        &self,
        name: &str,
        job: &str,
        failures: i64,
        next_attempt_at: i64,
    ) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "accounted_job = ?1, failures = ?2, next_attempt_at = ?3",
            &[&job, &failures, &next_attempt_at],
        )
    }

    /// A start that did not happen: why, and when to try again.
    pub fn note_managed_deferred(
        &self,
        name: &str,
        note: &str,
        next_attempt_at: i64,
    ) -> Result<bool, StoreError> {
        self.set_managed(
            name,
            "note = ?1, next_attempt_at = ?2",
            &[&note, &next_attempt_at],
        )
    }

    /// The unfinished run of a definition, when there is one.
    pub fn managed_running(&self, name: &str) -> Result<Option<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id FROM jobs WHERE managed = ?1 AND ended_at IS NULL LIMIT 1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)
    }

    /// The run of a definition that started last, while the history has it.
    pub fn managed_last(&self, name: &str) -> Result<Option<JobRecord>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row(
            "SELECT id, image, job_key, started_at, ended_at, exit_code, reason, callback_url, logs, oom_killed, cpu_ms, memory_peak, network_bytes, disk_bytes, pull_done, pull_total
             FROM jobs WHERE managed = ?1 ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [name],
            map_job,
        )
        .optional()
        .map_err(sql)
    }

    /// Paused now: until someone resumes (`1`), or until a time (seconds
    /// since 1970) that has not come yet. A pause whose time passed is over
    /// by itself; nothing has to clear it.
    pub fn draining_at(&self, now: i64) -> Result<bool, StoreError> {
        Ok(match self.drain_value()?.as_deref() {
            Some("1") => true,
            Some(value) => value.parse::<i64>().is_ok_and(|until| until > now),
            None => false,
        })
    }

    pub fn draining(&self) -> Result<bool, StoreError> {
        self.draining_at(crate::status::now_unix())
    }

    /// When a pause for a while ends. None for a pause until someone
    /// resumes, and when there is no pause.
    pub fn paused_until(&self, now: i64) -> Result<Option<i64>, StoreError> {
        Ok(self
            .drain_value()?
            .filter(|value| value != "1")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|until| *until > now))
    }

    fn drain_value(&self) -> Result<Option<String>, StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.query_row("SELECT value FROM meta WHERE key = 'draining'", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(sql)
    }

    pub fn set_draining(&self, draining: bool) -> Result<(), StoreError> {
        self.write_drain(if draining { "1" } else { "0" })
    }

    /// A pause that ends by itself at `until`.
    pub fn pause_until(&self, until: i64) -> Result<(), StoreError> {
        self.write_drain(&until.to_string())
    }

    fn write_drain(&self, value: &str) -> Result<(), StoreError> {
        let conn = self.conn.lock().expect("sqlite");
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('draining', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [value],
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
        oom_killed: row.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
        cpu_ms: row.get::<_, Option<i64>>(10)?.map(|v| v.max(0) as u64),
        memory_peak: row.get::<_, Option<i64>>(11)?.map(|v| v.max(0) as u64),
        network_bytes: row.get::<_, Option<i64>>(12)?.map(|v| v.max(0) as u64),
        disk_bytes: row.get::<_, Option<i64>>(13)?.map(|v| v.max(0) as u64),
        pull: match (
            row.get::<_, Option<i64>>(14)?,
            row.get::<_, Option<i64>>(15)?,
        ) {
            (Some(done), Some(total)) => Some((done.max(0) as u64, total.max(0) as u64)),
            _ => None,
        },
    })
}

const MANAGED_COLUMNS: &str = "name, spec, secrets, enabled, held, start_requested, \
     stop_requested, failures, accounted_job, last_job, last_started_at, next_attempt_at, note";

fn map_managed(row: &rusqlite::Row<'_>) -> rusqlite::Result<ManagedRow> {
    Ok(ManagedRow {
        name: row.get(0)?,
        spec: row.get(1)?,
        secrets: row.get(2)?,
        enabled: row.get(3)?,
        held: row.get(4)?,
        start_requested: row.get(5)?,
        stop_requested: row.get(6)?,
        failures: row.get(7)?,
        accounted_job: row.get(8)?,
        last_job: row.get(9)?,
        last_started_at: row.get(10)?,
        next_attempt_at: row.get(11)?,
        note: row.get(12)?,
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

/// A job with this image started at `at`: its last use.
fn note_image(conn: &Connection, image: &str, at: i64) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO images (image, last_used) VALUES (?1, ?2)
         ON CONFLICT(image) DO UPDATE SET last_used = MAX(last_used, ?2)",
        params![image, at],
    )
    .map_err(sql)?;
    Ok(())
}

/// What the daily pass over unused images did last.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cleanup {
    pub at: i64,
    pub images: u64,
    pub freed_bytes: u64,
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
            managed: None,
            memory_limit: None,
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

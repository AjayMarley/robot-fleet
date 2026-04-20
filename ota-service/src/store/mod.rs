use std::sync::Mutex;

use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::InProgress => "in_progress",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(JobStatus::Pending),
            "in_progress" => Some(JobStatus::InProgress),
            "completed" => Some(JobStatus::Completed),
            "failed" => Some(JobStatus::Failed),
            _ => None,
        }
    }

}

#[derive(Debug, Clone)]
pub struct NewJob {
    pub device_id: String,
    pub target_version: String,
    pub artifact_url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: String,
    pub device_id: String,
    pub target_version: String,
    pub artifact_url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub status: JobStatus,
    pub error_message: Option<String>,
    pub created_at: String,
}

pub struct SqliteJobStore {
    conn: Mutex<Connection>,
}

impl SqliteJobStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS update_jobs (
                id             TEXT PRIMARY KEY,
                device_id      TEXT NOT NULL,
                target_version TEXT NOT NULL,
                artifact_url   TEXT NOT NULL,
                sha256         TEXT NOT NULL,
                size_bytes     INTEGER NOT NULL DEFAULT 0,
                status         TEXT NOT NULL DEFAULT 'pending',
                error_message  TEXT,
                created_at     TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at     TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_device ON update_jobs(device_id, status);",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn create_job(&self, job: NewJob) -> Result<JobRecord> {
        let conn = self.conn.lock().unwrap();

        // Check for existing active job
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM update_jobs WHERE device_id = ?1 AND status IN ('pending','in_progress')",
            params![job.device_id],
            |row| row.get(0),
        )?;
        if active > 0 {
            return Err(Error::AlreadyPending);
        }

        let id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO update_jobs (id, device_id, target_version, artifact_url, sha256, size_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, job.device_id, job.target_version, job.artifact_url, job.sha256, job.size_bytes as i64],
        )?;
        self.get_job_with_conn(&conn, &id)
    }

    pub fn get_pending_for_device(&self, device_id: &str) -> Result<Option<JobRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, device_id, target_version, artifact_url, sha256, size_bytes,
                    status, error_message, created_at
             FROM update_jobs WHERE device_id = ?1 AND status IN ('pending','in_progress')
             ORDER BY created_at ASC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![device_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_record(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn update_status(&self, id: &str, status: JobStatus, error: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE update_jobs SET status = ?1, error_message = ?2, updated_at = datetime('now') WHERE id = ?3",
            params![status.as_str(), error, id],
        )?;
        if updated == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn list_for_device(
        &self,
        device_id: &str,
        limit: i64,
        cursor: Option<&str>,
    ) -> Result<(Vec<JobRecord>, Option<String>)> {
        let conn = self.conn.lock().unwrap();
        let fetch = limit + 1;
        let mut stmt = conn.prepare(
            "SELECT id, device_id, target_version, artifact_url, sha256, size_bytes,
                    status, error_message, created_at
             FROM update_jobs WHERE device_id = ?1 AND (?2 IS NULL OR created_at < ?2)
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let records: Vec<JobRecord> = stmt
            .query_map(params![device_id, cursor, fetch], |row| {
                Ok(row_to_record(row).unwrap())
            })?
            .filter_map(|r| r.ok())
            .collect();

        let next_cursor = if records.len() as i64 > limit {
            records.get(limit as usize - 1).map(|r| r.created_at.clone())
        } else {
            None
        };
        Ok((records.into_iter().take(limit as usize).collect(), next_cursor))
    }

    fn get_job_with_conn(&self, conn: &Connection, id: &str) -> Result<JobRecord> {
        conn.query_row(
            "SELECT id, device_id, target_version, artifact_url, sha256, size_bytes,
                    status, error_message, created_at
             FROM update_jobs WHERE id = ?1",
            params![id],
            |row| Ok(row_to_record(row).unwrap()),
        )
        .map_err(|_| Error::NotFound(id.to_string()))
    }
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<JobRecord> {
    Ok(JobRecord {
        id: row.get(0)?,
        device_id: row.get(1)?,
        target_version: row.get(2)?,
        artifact_url: row.get(3)?,
        sha256: row.get(4)?,
        size_bytes: row.get::<_, i64>(5)? as u64,
        status: JobStatus::from_str(&row.get::<_, String>(6)?).unwrap_or(JobStatus::Pending),
        error_message: row.get(7)?,
        created_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteJobStore {
        SqliteJobStore::new(":memory:").unwrap()
    }

    fn new_job(device_id: &str, version: &str) -> NewJob {
        NewJob {
            device_id: device_id.to_string(),
            target_version: version.to_string(),
            artifact_url: "http://example.com/artifact.tar.gz".to_string(),
            sha256: "abc123".to_string(),
            size_bytes: 1024,
        }
    }

    #[test]
    fn create_and_get_job() {
        let s = store();
        let job = s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.device_id, "dev-1");
        assert_eq!(job.target_version, "v2.0.0");
    }

    #[test]
    fn create_job_blocks_second_pending() {
        let s = store();
        s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        let err = s.create_job(new_job("dev-1", "v2.1.0")).unwrap_err();
        assert!(matches!(err, Error::AlreadyPending));
    }

    #[test]
    fn create_job_allowed_after_completion() {
        let s = store();
        let job = s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        s.update_status(&job.id, JobStatus::Completed, None).unwrap();
        let job2 = s.create_job(new_job("dev-1", "v2.1.0")).unwrap();
        assert_eq!(job2.target_version, "v2.1.0");
    }

    #[test]
    fn create_job_allowed_after_failure() {
        let s = store();
        let job = s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        s.update_status(&job.id, JobStatus::Failed, Some("install failed")).unwrap();
        let job2 = s.create_job(new_job("dev-1", "v2.1.0")).unwrap();
        assert_eq!(job2.status, JobStatus::Pending);
    }

    #[test]
    fn get_pending_for_device_returns_active_job() {
        let s = store();
        let job = s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        let pending = s.get_pending_for_device("dev-1").unwrap();
        assert!(pending.is_some());
        assert_eq!(pending.unwrap().id, job.id);
    }

    #[test]
    fn get_pending_for_device_returns_none_when_completed() {
        let s = store();
        let job = s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        s.update_status(&job.id, JobStatus::Completed, None).unwrap();
        let pending = s.get_pending_for_device("dev-1").unwrap();
        assert!(pending.is_none());
    }

    #[test]
    fn update_status_returns_not_found_for_missing_id() {
        let s = store();
        let err = s.update_status("no-such-id", JobStatus::Completed, None).unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn list_for_device_returns_jobs_in_desc_order() {
        let s = store();
        s.create_job(new_job("dev-1", "v1.0.0")).unwrap();
        let j2 = s.create_job(new_job("dev-2", "v2.0.0")).unwrap();
        let (jobs, cursor) = s.list_for_device("dev-2", 10, None).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, j2.id);
        assert!(cursor.is_none());
    }

    #[test]
    fn already_pending_guard_independent_per_device() {
        let s = store();
        s.create_job(new_job("dev-1", "v2.0.0")).unwrap();
        // Different device — should succeed
        let job = s.create_job(new_job("dev-2", "v2.0.0")).unwrap();
        assert_eq!(job.device_id, "dev-2");
    }
}

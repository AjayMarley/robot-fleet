use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Error;

// ── Domain types ─────────────────────────────────────────────────────────────

pub struct NewArtifact {
    pub model: String,
    pub version: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub channel: String,
    pub object_key: String,
    pub download_url: String,
}

#[derive(Debug)]
pub struct ArtifactRecord {
    pub id: String,
    pub model: String,
    pub version: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub channel: String,
    pub object_key: String,
    pub download_url: String,
    pub created_at: i64, // unix seconds
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Registry: Send + Sync {
    async fn create(&self, artifact: NewArtifact) -> Result<ArtifactRecord, Error>;
    async fn get(&self, id: &str) -> Result<ArtifactRecord, Error>;
    async fn get_latest(&self, model: &str, channel: &str) -> Result<ArtifactRecord, Error>;
    async fn list(&self, model: &str, channel: &str, limit: i64) -> Result<Vec<ArtifactRecord>, Error>;
}

// ── SQLite implementation ─────────────────────────────────────────────────────

pub struct SqliteRegistry {
    conn: Mutex<rusqlite::Connection>,
}

const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS artifacts (
    id           TEXT PRIMARY KEY,
    model        TEXT NOT NULL,
    version      TEXT NOT NULL,
    sha256       TEXT NOT NULL,
    size_bytes   INTEGER NOT NULL,
    channel      TEXT NOT NULL,
    object_key   TEXT NOT NULL,
    download_url TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    UNIQUE(model, version)
);
";

impl SqliteRegistry {
    pub fn new(db_path: &str) -> Result<Self, Error> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(MIGRATION)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    Ok(ArtifactRecord {
        id:           row.get(0)?,
        model:        row.get(1)?,
        version:      row.get(2)?,
        sha256:       row.get(3)?,
        size_bytes:   row.get(4)?,
        channel:      row.get(5)?,
        object_key:   row.get(6)?,
        download_url: row.get(7)?,
        created_at:   row.get(8)?,
    })
}

#[async_trait]
impl Registry for SqliteRegistry {
    async fn create(&self, a: NewArtifact) -> Result<ArtifactRecord, Error> {
        let id = Uuid::new_v4().to_string();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();

        let result = {
            let conn = self.conn.lock().map_err(|_| Error::Storage("db mutex poisoned".into()))?;
            conn.execute(
                "INSERT INTO artifacts
                 (id, model, version, sha256, size_bytes, channel, object_key, download_url, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id, a.model, a.version, a.sha256,
                    a.size_bytes, a.channel, a.object_key, a.download_url, now
                ],
            )
        };

        match result {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(Error::VersionConflict { model: a.model, version: a.version });
            }
            Err(e) => return Err(Error::Db(e)),
            Ok(_) => {}
        }

        Ok(ArtifactRecord {
            id,
            model: a.model,
            version: a.version,
            sha256: a.sha256,
            size_bytes: a.size_bytes,
            channel: a.channel,
            object_key: a.object_key,
            download_url: a.download_url,
            created_at: now,
        })
    }

    async fn get(&self, id: &str) -> Result<ArtifactRecord, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Storage("db mutex poisoned".into()))?;
        conn.query_row(
            "SELECT id, model, version, sha256, size_bytes, channel, object_key, download_url, created_at
             FROM artifacts WHERE id = ?1",
            [id],
            row_to_record,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(id.to_string()),
            other => Error::Db(other),
        })
    }

    async fn get_latest(&self, model: &str, channel: &str) -> Result<ArtifactRecord, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Storage("db mutex poisoned".into()))?;
        conn.query_row(
            "SELECT id, model, version, sha256, size_bytes, channel, object_key, download_url, created_at
             FROM artifacts WHERE model = ?1 AND channel = ?2
             ORDER BY created_at DESC LIMIT 1",
            [model, channel],
            row_to_record,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                Error::NotFound(format!("{model}@latest ({channel})"))
            }
            other => Error::Db(other),
        })
    }

    async fn list(&self, model: &str, channel: &str, limit: i64) -> Result<Vec<ArtifactRecord>, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Storage("db mutex poisoned".into()))?;
        let mut stmt = conn.prepare(
            "SELECT id, model, version, sha256, size_bytes, channel, object_key, download_url, created_at
             FROM artifacts WHERE model = ?1 AND channel = ?2
             ORDER BY created_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![model, channel, limit], row_to_record)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Error::Db)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> SqliteRegistry {
        SqliteRegistry::new(":memory:").unwrap()
    }

    fn new_artifact(model: &str, version: &str) -> NewArtifact {
        NewArtifact {
            model: model.into(),
            version: version.into(),
            sha256: "abc123".into(),
            size_bytes: 1024,
            channel: "stable".into(),
            object_key: format!("{model}/{version}/firmware.bin"),
            download_url: "https://example.com/fw".into(),
        }
    }

    #[tokio::test]
    async fn create_and_get() {
        let r = registry();
        let record = r.create(new_artifact("humanoid-v2", "1.0.0")).await.unwrap();
        assert!(!record.id.is_empty());

        let fetched = r.get(&record.id).await.unwrap();
        assert_eq!(fetched.version, "1.0.0");
        assert_eq!(fetched.model, "humanoid-v2");
    }

    #[tokio::test]
    async fn get_not_found() {
        let r = registry();
        let err = r.get("nonexistent-id").await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[tokio::test]
    async fn version_conflict() {
        let r = registry();
        r.create(new_artifact("humanoid-v2", "1.0.0")).await.unwrap();
        let err = r.create(new_artifact("humanoid-v2", "1.0.0")).await.unwrap_err();
        assert!(matches!(err, Error::VersionConflict { .. }));
    }

    #[tokio::test]
    async fn get_latest_returns_most_recent() {
        let r = registry();
        r.create(new_artifact("humanoid-v2", "1.0.0")).await.unwrap();
        // small sleep so created_at timestamps differ
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        r.create(new_artifact("humanoid-v2", "2.0.0")).await.unwrap();

        let latest = r.get_latest("humanoid-v2", "stable").await.unwrap();
        assert_eq!(latest.version, "2.0.0");
    }

    #[tokio::test]
    async fn list_filtered_by_model_and_channel() {
        let r = registry();
        r.create(new_artifact("humanoid-v2", "1.0.0")).await.unwrap();
        r.create(new_artifact("humanoid-v2", "2.0.0")).await.unwrap();
        // different model — should not appear
        r.create(NewArtifact {
            model: "arm-v1".into(),
            version: "1.0.0".into(),
            sha256: "def456".into(),
            size_bytes: 512,
            channel: "stable".into(),
            object_key: "arm/1.0.0/fw".into(),
            download_url: "https://example.com/arm".into(),
        })
        .await
        .unwrap();

        let results = r.list("humanoid-v2", "stable", 10).await.unwrap();
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|a| a.model == "humanoid-v2"));
    }
}

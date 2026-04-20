use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Error;

// ── Domain types ──────────────────────────────────────────────────────────────

pub struct NewDevice {
    pub serial: String,
    pub model: String,
    pub firmware: String,
    pub operational_cert_pem: String,
}

#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub id: String,
    pub serial: String,
    pub model: String,
    pub firmware: String,
    pub status: String,
    pub operational_cert_pem: String,
    pub enrolled_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone)]
pub struct PreEnrollmentRecord {
    pub serial: String,
    pub model: String,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait DeviceStore: Send + Sync {
    async fn create_device(&self, device: NewDevice) -> Result<DeviceRecord, Error>;
    async fn get_device(&self, id: &str) -> Result<DeviceRecord, Error>;
    async fn get_by_serial(&self, serial: &str) -> Result<DeviceRecord, Error>;
    async fn update_status(&self, id: &str, status: &str, reason: &str) -> Result<(), Error>;
    async fn touch_last_seen(&self, id: &str) -> Result<(), Error>;
    async fn list_devices(
        &self,
        model: Option<String>,
        status: Option<String>,
        limit: i64,
        cursor: Option<String>,
    ) -> Result<(Vec<DeviceRecord>, Option<String>), Error>;
    async fn register_serial(&self, serial: &str, model: &str) -> Result<(), Error>;
    async fn lookup_and_claim(&self, serial: &str) -> Result<PreEnrollmentRecord, Error>;
    async fn count_devices(&self) -> Result<u64, Error>;
    // ── Factory manifest (Phase 0 / Phase 1) ──
    async fn seed_manifest(&self, serial: &str, model: &str, token: &str) -> Result<(), Error>;
    async fn claim_manifest_entry(&self, serial: &str, token: &str) -> Result<String, Error>;
}

// ── SQLite implementation ─────────────────────────────────────────────────────

pub struct SqliteDeviceStore {
    conn: Mutex<rusqlite::Connection>,
}

const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS factory_manifest (
    serial         TEXT PRIMARY KEY,
    model          TEXT NOT NULL,
    token          TEXT NOT NULL,
    provisioned_at INTEGER
);

CREATE TABLE IF NOT EXISTS devices (
    id                   TEXT PRIMARY KEY,
    serial               TEXT NOT NULL UNIQUE,
    model                TEXT NOT NULL,
    firmware             TEXT NOT NULL,
    status               TEXT NOT NULL DEFAULT 'pending',
    operational_cert_pem TEXT NOT NULL,
    enrolled_at          INTEGER NOT NULL,
    last_seen_at         INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pre_enrollment (
    serial  TEXT PRIMARY KEY,
    model   TEXT NOT NULL,
    claimed INTEGER NOT NULL DEFAULT 0
);
";

impl SqliteDeviceStore {
    pub fn new(db_path: &str) -> Result<Self, Error> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute_batch(MIGRATION)?;
        Ok(Self { conn: Mutex::new(conn) })
    }
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRecord> {
    Ok(DeviceRecord {
        id:                   row.get(0)?,
        serial:               row.get(1)?,
        model:                row.get(2)?,
        firmware:             row.get(3)?,
        status:               row.get(4)?,
        operational_cert_pem: row.get(5)?,
        enrolled_at:          row.get(6)?,
        last_seen_at:         row.get(7)?,
    })
}

#[async_trait]
impl DeviceStore for SqliteDeviceStore {
    async fn create_device(&self, d: NewDevice) -> Result<DeviceRecord, Error> {
        let id = Uuid::new_v4().to_string();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        {
            let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
            conn.execute(
                "INSERT INTO devices (id, serial, model, firmware, status, operational_cert_pem, enrolled_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?6)",
                rusqlite::params![id, d.serial, d.model, d.firmware, d.operational_cert_pem, now],
            ).map_err(|e| match e {
                rusqlite::Error::SqliteFailure(ref fe, _)
                    if fe.code == rusqlite::ErrorCode::ConstraintViolation =>
                    Error::AlreadyEnrolled(d.serial.clone()),
                other => Error::Db(other),
            })?;
        }
        Ok(DeviceRecord {
            id,
            serial: d.serial,
            model: d.model,
            firmware: d.firmware,
            status: "active".into(),
            operational_cert_pem: d.operational_cert_pem,
            enrolled_at: now,
            last_seen_at: now,
        })
    }

    async fn get_device(&self, id: &str) -> Result<DeviceRecord, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        conn.query_row(
            "SELECT id, serial, model, firmware, status, operational_cert_pem, enrolled_at, last_seen_at
             FROM devices WHERE id = ?1",
            [id],
            row_to_device,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(id.to_string()),
            other => Error::Db(other),
        })
    }

    async fn get_by_serial(&self, serial: &str) -> Result<DeviceRecord, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        conn.query_row(
            "SELECT id, serial, model, firmware, status, operational_cert_pem, enrolled_at, last_seen_at
             FROM devices WHERE serial = ?1",
            [serial],
            row_to_device,
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(serial.to_string()),
            other => Error::Db(other),
        })
    }

    async fn update_status(&self, id: &str, status: &str, _reason: &str) -> Result<(), Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        let rows = conn.execute(
            "UPDATE devices SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, id],
        )?;
        if rows == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn touch_last_seen(&self, id: &str) -> Result<(), Error> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        let rows = conn.execute(
            "UPDATE devices SET last_seen_at = ?1 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
        if rows == 0 {
            return Err(Error::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn list_devices(
        &self,
        model: Option<String>,
        status: Option<String>,
        limit: i64,
        cursor: Option<String>,
    ) -> Result<(Vec<DeviceRecord>, Option<String>), Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;

        // Build query with optional filters
        let mut sql = String::from(
            "SELECT id, serial, model, firmware, status, operational_cert_pem, enrolled_at, last_seen_at
             FROM devices WHERE 1=1"
        );
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(m) = model {
            sql.push_str(&format!(" AND model = ?{}", params.len() + 1));
            params.push(Box::new(m));
        }
        if let Some(s) = status {
            sql.push_str(&format!(" AND status = ?{}", params.len() + 1));
            params.push(Box::new(s));
        }
        if let Some(c) = cursor {
            sql.push_str(&format!(" AND id > ?{}", params.len() + 1));
            params.push(Box::new(c));
        }
        sql.push_str(&format!(" ORDER BY id LIMIT ?{}", params.len() + 1));
        params.push(Box::new(limit + 1)); // fetch one extra to detect next page

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows: Vec<DeviceRecord> = stmt
            .query_map(param_refs.as_slice(), row_to_device)?
            .collect::<rusqlite::Result<_>>()?;

        let next_cursor = if rows.len() as i64 > limit {
            rows.pop(); // discard the extra lookahead row
            rows.last().map(|r| r.id.clone())
        } else {
            None
        };

        Ok((rows, next_cursor))
    }

    async fn register_serial(&self, serial: &str, model: &str) -> Result<(), Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        // Upsert: insert fresh or reset a stale claim (partial enrollment that never completed).
        // If the device is fully enrolled (row exists in `devices`), leave claimed=1 intact
        // so re-enrollment is still blocked.
        conn.execute(
            "INSERT INTO pre_enrollment (serial, model, claimed) VALUES (?1, ?2, 0)
             ON CONFLICT(serial) DO UPDATE SET claimed = 0
             WHERE NOT EXISTS (SELECT 1 FROM devices WHERE serial = excluded.serial)",
            rusqlite::params![serial, model],
        )?;
        Ok(())
    }

    async fn lookup_and_claim(&self, serial: &str) -> Result<PreEnrollmentRecord, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;

        // Single transaction: check unclaimed → claim → return
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let result: Result<PreEnrollmentRecord, Error> = (|| {
            let row = conn.query_row(
                "SELECT serial, model, claimed FROM pre_enrollment WHERE serial = ?1",
                [serial],
                |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(2)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotPreEnrolled(serial.to_string()),
                other => Error::Db(other),
            })?;

            let (model, claimed) = row;
            if claimed != 0 {
                return Err(Error::SerialAlreadyClaimed(serial.to_string()));
            }

            conn.execute(
                "UPDATE pre_enrollment SET claimed = 1 WHERE serial = ?1",
                [serial],
            )?;

            Ok(PreEnrollmentRecord { serial: serial.to_string(), model })
        })();

        match result {
            Ok(rec) => {
                conn.execute_batch("COMMIT")?;
                Ok(rec)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    async fn count_devices(&self) -> Result<u64, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    async fn seed_manifest(&self, serial: &str, model: &str, token: &str) -> Result<(), Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        conn.execute(
            "INSERT OR IGNORE INTO factory_manifest (serial, model, token) VALUES (?1, ?2, ?3)",
            rusqlite::params![serial, model, token],
        )?;
        Ok(())
    }

    async fn claim_manifest_entry(&self, serial: &str, token: &str) -> Result<String, Error> {
        let conn = self.conn.lock().map_err(|_| Error::Db(rusqlite::Error::InvalidQuery))?;
        conn.execute_batch("BEGIN IMMEDIATE")?;

        let result: Result<String, Error> = (|| {
            let row = conn.query_row(
                "SELECT model, token, provisioned_at FROM factory_manifest WHERE serial = ?1",
                [serial],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NotInManifest(serial.to_string()),
                other => Error::Db(other),
            })?;

            let (model, stored_token, provisioned_at) = row;
            if stored_token != token {
                return Err(Error::InvalidProvisionToken(serial.to_string()));
            }
            if provisioned_at.is_some() {
                return Err(Error::AlreadyProvisioned(serial.to_string()));
            }

            let now = time::OffsetDateTime::now_utc().unix_timestamp();
            conn.execute(
                "UPDATE factory_manifest SET provisioned_at = ?1 WHERE serial = ?2",
                rusqlite::params![now, serial],
            )?;

            // Seed pre_enrollment so Phase 2 bootstrap can proceed
            conn.execute(
                "INSERT OR IGNORE INTO pre_enrollment (serial, model, claimed) VALUES (?1, ?2, 0)",
                rusqlite::params![serial, model],
            )?;

            Ok(model)
        })();

        match result {
            Ok(model) => { conn.execute_batch("COMMIT")?; Ok(model) }
            Err(e)    => { let _ = conn.execute_batch("ROLLBACK"); Err(e) }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SqliteDeviceStore {
        SqliteDeviceStore::new(":memory:").unwrap()
    }

    fn new_device(serial: &str) -> NewDevice {
        NewDevice {
            serial: serial.into(),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            operational_cert_pem: "fake-cert".into(),
        }
    }

    #[tokio::test]
    async fn create_and_get_device() {
        let s = store();
        let rec = s.create_device(new_device("SN-001")).await.unwrap();
        assert_eq!(rec.serial, "SN-001");
        assert_eq!(rec.status, "active");

        let fetched = s.get_device(&rec.id).await.unwrap();
        assert_eq!(fetched.serial, "SN-001");
    }

    #[tokio::test]
    async fn get_by_serial() {
        let s = store();
        s.create_device(new_device("SN-002")).await.unwrap();
        let rec = s.get_by_serial("SN-002").await.unwrap();
        assert_eq!(rec.serial, "SN-002");
    }

    #[tokio::test]
    async fn create_duplicate_serial_returns_already_enrolled() {
        let s = store();
        s.create_device(new_device("SN-003")).await.unwrap();
        let err = s.create_device(new_device("SN-003")).await.unwrap_err();
        assert!(matches!(err, Error::AlreadyEnrolled(_)));
    }

    #[tokio::test]
    async fn get_not_found() {
        let s = store();
        let err = s.get_device("no-such-id").await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[tokio::test]
    async fn update_status_and_touch_last_seen() {
        let s = store();
        let rec = s.create_device(new_device("SN-004")).await.unwrap();

        s.update_status(&rec.id, "suspended", "maintenance").await.unwrap();
        let updated = s.get_device(&rec.id).await.unwrap();
        assert_eq!(updated.status, "suspended");

        s.touch_last_seen(&rec.id).await.unwrap();
        let touched = s.get_device(&rec.id).await.unwrap();
        assert!(touched.last_seen_at >= rec.last_seen_at);
    }

    #[tokio::test]
    async fn list_devices_with_filters() {
        let s = store();
        s.create_device(new_device("SN-010")).await.unwrap();
        s.create_device(new_device("SN-011")).await.unwrap();
        s.create_device(NewDevice {
            serial: "SN-012".into(),
            model: "arm-v1".into(),
            firmware: "0.1.0".into(),
            operational_cert_pem: "cert".into(),
        })
        .await
        .unwrap();

        let (all, _) = s.list_devices(None, None, 10, None).await.unwrap();
        assert_eq!(all.len(), 3);

        let (filtered, _) = s.list_devices(Some("humanoid-v2".into()), None, 10, None).await.unwrap();
        assert_eq!(filtered.len(), 2);
    }

    #[tokio::test]
    async fn list_devices_pagination() {
        let s = store();
        for i in 0..5u8 {
            s.create_device(new_device(&format!("SN-PAG-{i:03}"))).await.unwrap();
        }

        let (page1, cursor) = s.list_devices(None, None, 3, None).await.unwrap();
        assert_eq!(page1.len(), 3);
        assert!(cursor.is_some());

        let (page2, cursor2) = s.list_devices(None, None, 3, cursor).await.unwrap();
        assert_eq!(page2.len(), 2);
        assert!(cursor2.is_none());
    }

    #[tokio::test]
    async fn register_and_claim_serial() {
        let s = store();
        s.register_serial("SN-100", "humanoid-v2").await.unwrap();
        let rec = s.lookup_and_claim("SN-100").await.unwrap();
        assert_eq!(rec.serial, "SN-100");
        assert_eq!(rec.model, "humanoid-v2");
    }

    #[tokio::test]
    async fn claim_unknown_serial_returns_not_pre_enrolled() {
        let s = store();
        let err = s.lookup_and_claim("SN-GHOST").await.unwrap_err();
        assert!(matches!(err, Error::NotPreEnrolled(_)));
    }

    #[tokio::test]
    async fn claim_already_claimed_serial_returns_error() {
        let s = store();
        s.register_serial("SN-200", "humanoid-v2").await.unwrap();
        s.lookup_and_claim("SN-200").await.unwrap();
        let err = s.lookup_and_claim("SN-200").await.unwrap_err();
        assert!(matches!(err, Error::SerialAlreadyClaimed(_)));
    }

    #[tokio::test]
    async fn concurrent_claim_only_one_succeeds() {
        use std::sync::Arc;

        let s = Arc::new(store());
        s.register_serial("SN-RACE", "humanoid-v2").await.unwrap();

        let s1 = s.clone();
        let s2 = s.clone();
        let t1 = tokio::spawn(async move { s1.lookup_and_claim("SN-RACE").await });
        let t2 = tokio::spawn(async move { s2.lookup_and_claim("SN-RACE").await });

        let (r1, r2) = tokio::join!(t1, t2);
        let results = [r1.unwrap(), r2.unwrap()];
        let successes = results.iter().filter(|r| r.is_ok()).count();
        let failures = results.iter().filter(|r| r.is_err()).count();
        assert_eq!(successes, 1, "exactly one claim must succeed");
        assert_eq!(failures, 1, "exactly one claim must fail");
    }
}

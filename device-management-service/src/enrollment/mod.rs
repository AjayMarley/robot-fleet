use std::sync::Arc;

use rustls::pki_types::CertificateDer;

use crate::error::Error;
use crate::store::{DeviceStore, NewDevice};

#[derive(Debug)]
pub struct EnrollResult {
    pub device_id: String,
    pub operational_cert_pem: String,
    pub fleet_ca_chain_pem: String,
    pub expires_at: i64,
}

pub struct EnrollmentService<S: DeviceStore> {
    store: Arc<S>,
    fleet_ca_cert_pem: String,
    fleet_ca_key_pem: String,
    cert_validity_days: u32,
}

impl<S: DeviceStore> EnrollmentService<S> {
    pub fn new(
        store: Arc<S>,
        fleet_ca_cert_pem: String,
        fleet_ca_key_pem: String,
        cert_validity_days: u32,
    ) -> Self {
        Self { store, fleet_ca_cert_pem, fleet_ca_key_pem, cert_validity_days }
    }

    pub async fn bootstrap_enroll(
        &self,
        serial_from_req: &str,
        csr_pem: &str,
        firmware: &str,
        peer_certs: &[CertificateDer<'_>],
        device_ca_pem: &str,
    ) -> Result<EnrollResult, Error> {
        // 1. Extract serial from peer cert CN
        let cert_serial = robot_fleet_pki::mtls::extract_serial(peer_certs)?;

        // 2. Cross-validate serial if provided in request
        if !serial_from_req.is_empty() && serial_from_req != cert_serial {
            return Err(Error::SerialMismatch {
                cert: cert_serial,
                request: serial_from_req.to_string(),
            });
        }

        // 3. Verify cert is signed by Device CA
        robot_fleet_pki::mtls::verify_signed_by(peer_certs, device_ca_pem)
            .map_err(|e| Error::CertVerification(e.to_string()))?;

        // 4. Atomic claim — prevents replay
        let pre = self.store.lookup_and_claim(&cert_serial).await?;

        // 5. Sign the CSR with Fleet CA
        let signed = robot_fleet_pki::csr::sign_csr(
            csr_pem,
            &self.fleet_ca_cert_pem,
            &self.fleet_ca_key_pem,
            self.cert_validity_days,
        )?;

        let expires_at = time::OffsetDateTime::now_utc().unix_timestamp()
            + (self.cert_validity_days as i64 * 86_400);

        // 6. Persist device record
        let record = self
            .store
            .create_device(NewDevice {
                serial: cert_serial,
                model: pre.model,
                firmware: firmware.to_string(),
                operational_cert_pem: signed.clone(),
            })
            .await?;

        tracing::info!(
            device_id    = %record.id,
            serial       = %record.serial,
            model        = %record.model,
            firmware     = %firmware,
            cert_expires_at = %expires_at,
            port         = 8444,
            "[Phase 2 complete] bootstrap enrollment — operational cert issued (Fleet CA signed)"
        );

        Ok(EnrollResult {
            device_id: record.id,
            operational_cert_pem: signed,
            fleet_ca_chain_pem: self.fleet_ca_cert_pem.clone(),
            expires_at,
        })
    }

    pub async fn renew_cert(
        &self,
        device_id: &str,
        csr_pem: &str,
    ) -> Result<(String, i64), Error> {
        let device = self.store.get_device(device_id).await?;
        if device.status != "active" {
            return Err(Error::DeviceNotActive { status: device.status });
        }

        let signed = robot_fleet_pki::csr::sign_csr(
            csr_pem,
            &self.fleet_ca_cert_pem,
            &self.fleet_ca_key_pem,
            self.cert_validity_days,
        )?;

        let expires_at = time::OffsetDateTime::now_utc().unix_timestamp()
            + (self.cert_validity_days as i64 * 86_400);

        Ok((signed, expires_at))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rcgen::{CertificateParams, KeyPair};

    use crate::store::MockDeviceStore;

    // Build a minimal Device CA + leaf cert signed by it, returning PEM strings
    fn make_device_ca_and_leaf(serial: &str) -> (String, String, String) {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec![]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).unwrap();
        let ca_pem = ca_cert.pem();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec![serial.to_string()]).unwrap();
        leaf_params.distinguished_name.push(
            rcgen::DnType::CommonName,
            serial,
        );
        let leaf_cert = leaf_params.signed_by(&leaf_key, &ca_cert, &ca_key).unwrap();
        let leaf_pem = leaf_cert.pem();

        (ca_pem, leaf_pem, leaf_key.serialize_pem())
    }

    fn make_fleet_ca() -> (String, String) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn make_csr() -> String {
        let key = KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["test-device".to_string()]).unwrap();
        let csr = params.serialize_request(&key).unwrap();
        csr.pem().unwrap()
    }

    fn leaf_der(leaf_pem: &str) -> Vec<CertificateDer<'static>> {
        use rustls_pemfile::certs;
        let mut reader = std::io::BufReader::new(leaf_pem.as_bytes());
        certs(&mut reader).map(|r| r.unwrap()).collect()
    }

    #[tokio::test]
    async fn bootstrap_enroll_success() {
        let serial = "SN-ENROLL-001";
        let (device_ca_pem, leaf_pem, _leaf_key) = make_device_ca_and_leaf(serial);
        let (fleet_ca_pem, fleet_ca_key) = make_fleet_ca();
        let csr = make_csr();

        let mut mock = MockDeviceStore::new();
        mock.expect_lookup_and_claim()
            .returning(|s| Ok(crate::store::PreEnrollmentRecord {
                serial: s.to_string(),
                model: "humanoid-v2".into(),
            }));
        mock.expect_create_device()
            .returning(|d| Ok(crate::store::DeviceRecord {
                id: "dev-uuid-1".into(),
                serial: d.serial,
                model: d.model,
                firmware: d.firmware,
                status: "active".into(),
                operational_cert_pem: d.operational_cert_pem,
                enrolled_at: 0,
                last_seen_at: 0,
            }));

        let svc = EnrollmentService::new(
            Arc::new(mock),
            fleet_ca_pem,
            fleet_ca_key,
            30,
        );

        let certs = leaf_der(&leaf_pem);
        let result = svc
            .bootstrap_enroll("", &csr, "1.0.0", &certs, &device_ca_pem)
            .await
            .unwrap();

        assert_eq!(result.device_id, "dev-uuid-1");
        assert!(result.operational_cert_pem.contains("CERTIFICATE"));
    }

    #[tokio::test]
    async fn bootstrap_enroll_serial_mismatch_rejected() {
        let serial = "SN-REAL";
        let (device_ca_pem, leaf_pem, _) = make_device_ca_and_leaf(serial);
        let (fleet_ca_pem, fleet_ca_key) = make_fleet_ca();
        let csr = make_csr();

        let svc = EnrollmentService::new(
            Arc::new(MockDeviceStore::new()),
            fleet_ca_pem,
            fleet_ca_key,
            30,
        );

        let certs = leaf_der(&leaf_pem);
        let err = svc
            .bootstrap_enroll("SN-WRONG", &csr, "1.0.0", &certs, &device_ca_pem)
            .await
            .unwrap_err();

        assert!(matches!(err, Error::SerialMismatch { .. }));
    }

    #[tokio::test]
    async fn bootstrap_enroll_wrong_ca_rejected() {
        let serial = "SN-WRONGCA";
        let (_, leaf_pem, _) = make_device_ca_and_leaf(serial);
        let (wrong_ca_pem, _) = make_fleet_ca(); // different CA
        let (fleet_ca_pem, fleet_ca_key) = make_fleet_ca();
        let csr = make_csr();

        let svc = EnrollmentService::new(
            Arc::new(MockDeviceStore::new()),
            fleet_ca_pem,
            fleet_ca_key,
            30,
        );

        let certs = leaf_der(&leaf_pem);
        let err = svc
            .bootstrap_enroll("", &csr, "1.0.0", &certs, &wrong_ca_pem)
            .await
            .unwrap_err();

        assert!(matches!(err, Error::CertVerification(_)));
    }

    #[tokio::test]
    async fn renew_cert_inactive_device_rejected() {
        let (fleet_ca_pem, fleet_ca_key) = make_fleet_ca();
        let csr = make_csr();

        let mut mock = MockDeviceStore::new();
        mock.expect_get_device().returning(|id| Ok(crate::store::DeviceRecord {
            id: id.to_string(),
            serial: "SN-X".into(),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            status: "suspended".into(),
            operational_cert_pem: "cert".into(),
            enrolled_at: 0,
            last_seen_at: 0,
        }));

        let svc = EnrollmentService::new(Arc::new(mock), fleet_ca_pem, fleet_ca_key, 30);
        let err = svc.renew_cert("dev-id", &csr).await.unwrap_err();
        assert!(matches!(err, Error::DeviceNotActive { .. }));
    }
}

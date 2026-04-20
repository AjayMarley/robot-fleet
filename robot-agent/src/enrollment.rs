use std::path::PathBuf;

use tonic::transport::Channel;
use tracing::info;

use robot_fleet_pki::csr;
use robot_fleet_proto::fleet::v1::{
    device_management_service_client::DeviceManagementServiceClient, BootstrapEnrollRequest,
};

use crate::error::Result;

/// Manages the robot's enrollment state and operational credentials.
pub struct EnrollmentManager {
    pub cert_dir: PathBuf,
    pub device_cert_pem: String,
    pub device_key_pem: String,
    pub device_ca_pem: String,
    pub serial: String,
    pub model: String,
    pub firmware: String,
}

impl EnrollmentManager {
    /// Returns true if the operational cert exists on disk.
    pub fn is_enrolled(&self) -> bool {
        self.operational_cert_path().exists()
    }

    /// Reads operational cert + key from disk.
    pub fn load_operational_creds(&self) -> Result<(String, String)> {
        let cert = std::fs::read_to_string(self.operational_cert_path())?;
        let key = std::fs::read_to_string(self.operational_key_path())?;
        Ok((cert, key))
    }

    /// Performs bootstrap enrollment against the device-management bootstrap endpoint.
    /// Generates an ECDSA keypair, sends CSR, writes operational cert + fleet CA to disk.
    pub async fn enroll(&self, bootstrap_channel: Channel) -> Result<()> {
        info!("Starting bootstrap enrollment for serial={}", self.serial);

        let key_path = self.operational_key_path();
        let csr_pem = csr::generate_csr(&self.serial, &key_path)?;
        info!("CSR generated, key written to {}", key_path.display());

        let mut client = DeviceManagementServiceClient::new(bootstrap_channel);
        let response = client
            .bootstrap_enroll(BootstrapEnrollRequest {
                csr_pem,
                serial: self.serial.clone(),
                model: self.model.clone(),
                firmware: self.firmware.clone(),
                labels: Default::default(),
            })
            .await?
            .into_inner();

        std::fs::write(self.operational_cert_path(), &response.operational_cert_pem)?;
        std::fs::write(self.fleet_ca_path(), &response.fleet_ca_chain_pem)?;

        info!(
            device_id = %response.device_id,
            "Bootstrap enrollment succeeded"
        );
        Ok(())
    }

    pub fn fleet_ca_pem(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.fleet_ca_path())?)
    }

    fn operational_cert_path(&self) -> PathBuf {
        self.cert_dir.join("operational.pem")
    }

    fn operational_key_path(&self) -> PathBuf {
        self.cert_dir.join("operational.key")
    }

    fn fleet_ca_path(&self) -> PathBuf {
        self.cert_dir.join("fleet-ca-chain.pem")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn make_manager(dir: &Path) -> EnrollmentManager {
        EnrollmentManager {
            cert_dir: dir.to_path_buf(),
            device_cert_pem: String::new(),
            device_key_pem: String::new(),
            device_ca_pem: String::new(),
            serial: "SN-TEST-001".into(),
            model: "test-robot".into(),
            firmware: "0.1.0".into(),
        }
    }

    #[test]
    fn not_enrolled_when_cert_absent() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path());
        assert!(!mgr.is_enrolled());
    }

    #[test]
    fn enrolled_when_cert_present() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("operational.pem"), "fake-cert").unwrap();
        let mgr = make_manager(tmp.path());
        assert!(mgr.is_enrolled());
    }

    #[test]
    fn load_operational_creds_returns_cert_and_key() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("operational.pem"), "cert-data").unwrap();
        std::fs::write(tmp.path().join("operational.key"), "key-data").unwrap();
        let mgr = make_manager(tmp.path());
        let (cert, key) = mgr.load_operational_creds().unwrap();
        assert_eq!(cert, "cert-data");
        assert_eq!(key, "key-data");
    }

    #[test]
    fn load_operational_creds_errors_when_missing() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path());
        assert!(mgr.load_operational_creds().is_err());
    }

    #[test]
    fn fleet_ca_pem_reads_pinned_ca() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("fleet-ca-chain.pem"), "ca-chain").unwrap();
        let mgr = make_manager(tmp.path());
        assert_eq!(mgr.fleet_ca_pem().unwrap(), "ca-chain");
    }
}

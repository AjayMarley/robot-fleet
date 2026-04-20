use std::path::PathBuf;

use tonic::transport::Channel;
use tracing::info;

use robot_fleet_pki::csr;
use robot_fleet_proto::fleet::v1::{
    device_management_service_client::DeviceManagementServiceClient, BootstrapEnrollRequest,
};

use crate::error::Result;

pub struct EnrollmentManager {
    pub cert_dir: PathBuf,
    pub serial: String,
    pub model: String,
    pub firmware: String,
}

impl EnrollmentManager {
    /// True once Phase 2 bootstrap enrollment has completed.
    pub fn is_enrolled(&self) -> bool {
        self.operational_cert_path().exists()
    }

    /// True once Phase 1 provisioning has completed.
    pub fn is_provisioned(&self) -> bool {
        self.device_cert_path().exists()
    }

    /// Returns `(device_cert_pem, device_key_pem)` written by Phase 1.
    pub fn load_device_cert(&self) -> Result<(String, String)> {
        Ok((
            std::fs::read_to_string(self.device_cert_path())?,
            std::fs::read_to_string(self.device_key_path())?,
        ))
    }

    /// Returns `(operational_cert_pem, operational_key_pem)` written by Phase 2.
    pub fn load_operational_creds(&self) -> Result<(String, String)> {
        Ok((
            std::fs::read_to_string(self.operational_cert_path())?,
            std::fs::read_to_string(self.operational_key_path())?,
        ))
    }

    /// Returns the server-assigned UUID written during Phase 2.
    pub fn load_device_id(&self) -> Result<String> {
        Ok(std::fs::read_to_string(self.device_id_path())?)
    }

    /// Phase 2 — bootstrap enrollment.
    /// Uses the device cert (from Phase 1) as the mTLS client cert.
    /// Generates an operational keypair, sends CSR, stores the signed operational cert.
    pub async fn enroll(&self, bootstrap_channel: Channel) -> Result<()> {
        info!(serial = %self.serial, port = 8444, "[Phase 2] starting bootstrap enrollment — connecting with device cert (mTLS, Device CA trust)");

        let op_key_path = self.operational_key_path();
        let csr_pem = csr::generate_csr(&self.serial, &op_key_path)?;
        info!(serial = %self.serial, "[Phase 2] operational keypair generated, CSR ready for Fleet CA signing");

        let mut client = DeviceManagementServiceClient::new(bootstrap_channel);
        let response = client
            .bootstrap_enroll(BootstrapEnrollRequest {
                csr_pem,
                serial:   self.serial.clone(),
                model:    self.model.clone(),
                firmware: self.firmware.clone(),
                labels:   Default::default(),
            })
            .await?
            .into_inner();

        std::fs::write(self.operational_cert_path(), &response.operational_cert_pem)?;
        std::fs::write(self.fleet_ca_path(), &response.fleet_ca_chain_pem)?;
        std::fs::write(self.device_id_path(), &response.device_id)?;

        info!(device_id = %response.device_id, serial = %self.serial, "[Phase 2 complete] Fleet CA-signed operational cert stored — device admitted to fleet");
        Ok(())
    }

    fn device_cert_path(&self)      -> PathBuf { self.cert_dir.join("device.pem") }
    fn device_key_path(&self)       -> PathBuf { self.cert_dir.join("device.key") }
    fn operational_cert_path(&self) -> PathBuf { self.cert_dir.join("operational.pem") }
    fn operational_key_path(&self)  -> PathBuf { self.cert_dir.join("operational.key") }
    fn fleet_ca_path(&self)         -> PathBuf { self.cert_dir.join("fleet-ca-chain.pem") }
    fn device_id_path(&self)        -> PathBuf { self.cert_dir.join("device-id") }
}

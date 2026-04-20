use std::path::Path;

use tonic::transport::{Certificate, Channel, ClientTlsConfig};
use tracing::info;

use robot_fleet_pki::csr;
use robot_fleet_proto::fleet::v1::{
    provisioning_service_client::ProvisioningServiceClient, ProvisionDeviceRequest,
};

use crate::error::Result;

/// Phase 1 — factory provisioning.
///
/// Generates an ECDSA keypair (representing TPM key generation), sends the CSR
/// to the provisioning service with the one-time token, and stores the signed
/// device cert on disk. After this returns the device is ready for Phase 2
/// bootstrap enrollment.
pub async fn provision(
    serial: &str,
    token: &str,
    provisioning_addr: &str,
    fleet_ca_pem: &str,
    cert_dir: &Path,
) -> Result<()> {
    info!(serial, port = 8445, "[Phase 1] starting factory provisioning — connecting to ProvisioningService (TLS-only, token auth)");

    let key_path  = cert_dir.join("device.key");
    let cert_path = cert_dir.join("device.pem");

    let csr_pem = csr::generate_csr(serial, &key_path)?;
    info!(serial, "[Phase 1] ECDSA keypair generated on-device, CSR ready — private key never leaves device");

    // TLS-only (no client cert — device has nothing yet).
    // Server cert is signed by Fleet CA, which is the one public cert the device ships with.
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(fleet_ca_pem));

    let channel = Channel::from_shared(provisioning_addr.to_string())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()))?
        .tls_config(tls)?
        .connect()
        .await?;

    let mut client = ProvisioningServiceClient::new(channel);
    let resp = client
        .provision_device(ProvisionDeviceRequest {
            serial:          serial.to_string(),
            csr_pem,
            provision_token: token.to_string(),
        })
        .await?
        .into_inner();

    std::fs::write(&cert_path, &resp.device_cert_pem)?;
    info!(serial, "[Phase 1 complete] Device CA-signed cert stored on disk — ready for Phase 2 bootstrap");

    Ok(())
}

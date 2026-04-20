use std::sync::Arc;

use tonic::{Request, Response, Status};
use tracing::info;

use robot_fleet_proto::fleet::v1::{
    provisioning_service_server::ProvisioningService,
    ProvisionDeviceRequest, ProvisionDeviceResponse,
};

use crate::error::Error;
use crate::store::DeviceStore;

pub struct ProvisioningServer<S: DeviceStore> {
    store: Arc<S>,
    device_ca_cert_pem: String,
    device_ca_key_pem: String,
}

impl<S: DeviceStore> ProvisioningServer<S> {
    pub fn new(store: Arc<S>, device_ca_cert_pem: String, device_ca_key_pem: String) -> Self {
        Self { store, device_ca_cert_pem, device_ca_key_pem }
    }
}

fn map_err(e: Error) -> Status {
    match e {
        Error::NotInManifest(m)        => Status::failed_precondition(m),
        Error::InvalidProvisionToken(m) => Status::permission_denied(m),
        Error::AlreadyProvisioned(m)   => Status::already_exists(m),
        _ => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl<S: DeviceStore + 'static> ProvisioningService for ProvisioningServer<S> {
    async fn provision_device(
        &self,
        request: Request<ProvisionDeviceRequest>,
    ) -> Result<Response<ProvisionDeviceResponse>, Status> {
        let req = request.into_inner();

        if req.serial.is_empty() || req.csr_pem.is_empty() || req.provision_token.is_empty() {
            return Err(Status::invalid_argument("serial, csr_pem, and provision_token are required"));
        }

        // Validate token and atomically claim the manifest entry.
        // On success also seeds pre_enrollment so Phase 2 can proceed.
        let model = self.store
            .claim_manifest_entry(&req.serial, &req.provision_token)
            .await
            .map_err(map_err)?;

        // Sign the device CSR with the Device CA.
        let device_cert_pem = robot_fleet_pki::csr::sign_csr(
            &req.csr_pem,
            &self.device_ca_cert_pem,
            &self.device_ca_key_pem,
            365,
        )
        .map_err(|e| Status::internal(format!("CSR signing failed: {e}")))?;

        info!(
            serial = %req.serial,
            model  = %model,
            port   = 8445,
            "[Phase 1 complete] device cert issued (Device CA signed) — pre-enrollment seeded for Phase 2"
        );

        Ok(Response::new(ProvisionDeviceResponse { device_cert_pem }))
    }
}

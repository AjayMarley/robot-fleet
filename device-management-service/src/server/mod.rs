use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
#[allow(unused_imports)]
use tokio_stream::StreamExt as _;

use robot_fleet_proto::fleet::v1::{
    device_management_service_server::DeviceManagementService,
    BootstrapEnrollRequest, BootstrapEnrollResponse, DeviceInfo, GetDeviceRequest,
    HeartbeatRequest, HeartbeatResponse, ListDevicesRequest, ListDevicesResponse,
    RenewCertRequest, RenewCertResponse, UpdateDeviceStatusRequest, UpdateDeviceStatusResponse,
};

use crate::enrollment::EnrollmentService;
use crate::error::Error;
use crate::heartbeat::HeartbeatHandler;
use crate::store::{DeviceRecord, DeviceStore};

pub struct DeviceManagementServer<S: DeviceStore> {
    enrollment: Arc<EnrollmentService<S>>,
    heartbeat: Arc<HeartbeatHandler<S>>,
    store: Arc<S>,
    device_ca_pem: String,
}

impl<S: DeviceStore> DeviceManagementServer<S> {
    pub fn new(
        store: Arc<S>,
        fleet_ca_cert_pem: String,
        fleet_ca_key_pem: String,
        device_ca_pem: String,
        cert_validity_days: u32,
    ) -> Self {
        let enrollment = Arc::new(EnrollmentService::new(
            store.clone(),
            fleet_ca_cert_pem,
            fleet_ca_key_pem,
            cert_validity_days,
        ));
        let heartbeat = Arc::new(HeartbeatHandler::new(store.clone()));
        Self { enrollment, heartbeat, store, device_ca_pem }
    }
}

fn map_err(e: Error) -> Status {
    match e {
        Error::NotFound(m) => Status::not_found(m),
        Error::AlreadyEnrolled(m) => Status::already_exists(m),
        Error::SerialAlreadyClaimed(m) => Status::already_exists(m),
        Error::NotPreEnrolled(m) => Status::failed_precondition(m),
        Error::SerialMismatch { .. } => Status::invalid_argument(e.to_string()),
        Error::DeviceNotActive { .. } => Status::failed_precondition(e.to_string()),
        Error::CertVerification(m) => Status::unauthenticated(m),
        _ => Status::internal(e.to_string()),
    }
}

fn record_to_proto(r: DeviceRecord) -> DeviceInfo {
    DeviceInfo {
        device_id: r.id,
        serial: r.serial,
        model: r.model,
        firmware: r.firmware,
        status: r.status,
        enrolled_at: Some(prost_types::Timestamp { seconds: r.enrolled_at, nanos: 0 }),
        last_seen_at: Some(prost_types::Timestamp { seconds: r.last_seen_at, nanos: 0 }),
        labels: Default::default(),
    }
}

#[tonic::async_trait]
impl<S: DeviceStore + 'static> DeviceManagementService for DeviceManagementServer<S> {
    async fn bootstrap_enroll(
        &self,
        request: Request<BootstrapEnrollRequest>,
    ) -> Result<Response<BootstrapEnrollResponse>, Status> {
        let peer_certs = request
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("no peer certificate"))?;

        let req = request.into_inner();
        let result = self
            .enrollment
            .bootstrap_enroll(
                &req.serial,
                &req.csr_pem,
                &req.firmware,
                peer_certs.as_ref(),
                &self.device_ca_pem,
            )
            .await
            .map_err(map_err)?;

        Ok(Response::new(BootstrapEnrollResponse {
            device_id: result.device_id,
            operational_cert_pem: result.operational_cert_pem,
            fleet_ca_chain_pem: result.fleet_ca_chain_pem,
            cert_expires_at: Some(prost_types::Timestamp {
                seconds: result.expires_at,
                nanos: 0,
            }),
        }))
    }

    async fn renew_cert(
        &self,
        request: Request<RenewCertRequest>,
    ) -> Result<Response<RenewCertResponse>, Status> {
        let req = request.into_inner();
        let (cert_pem, expires_at) = self
            .enrollment
            .renew_cert(&req.device_id, &req.csr_pem)
            .await
            .map_err(map_err)?;

        Ok(Response::new(RenewCertResponse {
            operational_cert_pem: cert_pem,
            cert_expires_at: Some(prost_types::Timestamp { seconds: expires_at, nanos: 0 }),
        }))
    }

    async fn get_device(
        &self,
        request: Request<GetDeviceRequest>,
    ) -> Result<Response<DeviceInfo>, Status> {
        let record = self
            .store
            .get_device(&request.into_inner().device_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(record_to_proto(record)))
    }

    async fn list_devices(
        &self,
        request: Request<ListDevicesRequest>,
    ) -> Result<Response<ListDevicesResponse>, Status> {
        let req = request.into_inner();
        let limit = if req.limit > 0 { req.limit as i64 } else { 50 };
        let model = if req.model.is_empty() { None } else { Some(req.model) };
        let status = if req.status.is_empty() { None } else { Some(req.status) };
        let cursor = if req.cursor.is_empty() { None } else { Some(req.cursor) };

        let (records, next_cursor) = self
            .store
            .list_devices(model, status, limit, cursor)
            .await
            .map_err(map_err)?;

        Ok(Response::new(ListDevicesResponse {
            devices: records.into_iter().map(record_to_proto).collect(),
            next_cursor: next_cursor.unwrap_or_default(),
        }))
    }

    async fn update_device_status(
        &self,
        request: Request<UpdateDeviceStatusRequest>,
    ) -> Result<Response<UpdateDeviceStatusResponse>, Status> {
        let req = request.into_inner();
        self.store
            .update_status(&req.device_id, &req.status, &req.reason)
            .await
            .map_err(map_err)?;
        let record = self.store.get_device(&req.device_id).await.map_err(map_err)?;
        Ok(Response::new(UpdateDeviceStatusResponse {
            device: Some(record_to_proto(record)),
        }))
    }

    type StreamHeartbeatStream = ReceiverStream<Result<HeartbeatResponse, Status>>;

    async fn stream_heartbeat(
        &self,
        request: Request<Streaming<HeartbeatRequest>>,
    ) -> Result<Response<Self::StreamHeartbeatStream>, Status> {
        let (tx, rx) = mpsc::channel(32);
        let stream = request.into_inner();
        let heartbeat = self.heartbeat.clone();

        tokio::spawn(async move {
            heartbeat.handle(stream, tx).await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MockDeviceStore, NewDevice, PreEnrollmentRecord};
    use rcgen::{CertificateParams, KeyPair};

    fn make_fleet_ca() -> (String, String) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(vec![]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn make_server(store: MockDeviceStore) -> DeviceManagementServer<MockDeviceStore> {
        let (fleet_ca_pem, fleet_ca_key) = make_fleet_ca();
        DeviceManagementServer::new(
            Arc::new(store),
            fleet_ca_pem,
            fleet_ca_key,
            "fake-device-ca".into(),
            30,
        )
    }

    fn sample_record(id: &str) -> crate::store::DeviceRecord {
        crate::store::DeviceRecord {
            id: id.into(),
            serial: "SN-TEST".into(),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            status: "active".into(),
            operational_cert_pem: "cert".into(),
            enrolled_at: 0,
            last_seen_at: 0,
        }
    }

    #[tokio::test]
    async fn get_device_returns_device_info() {
        let mut mock = MockDeviceStore::new();
        mock.expect_get_device()
            .returning(|id| Ok(sample_record(id)));

        let server = make_server(mock);
        let resp = server
            .get_device(Request::new(GetDeviceRequest { device_id: "dev-abc".into() }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.device_id, "dev-abc");
        assert_eq!(resp.status, "active");
    }

    #[tokio::test]
    async fn get_device_not_found_returns_not_found_status() {
        let mut mock = MockDeviceStore::new();
        mock.expect_get_device()
            .returning(|id| Err(Error::NotFound(id.to_string())));

        let server = make_server(mock);
        let err = server
            .get_device(Request::new(GetDeviceRequest { device_id: "ghost".into() }))
            .await
            .unwrap_err();

        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn update_device_status_returns_updated_device() {
        let mut mock = MockDeviceStore::new();
        mock.expect_update_status().returning(|_, _, _| Ok(()));
        mock.expect_get_device().returning(|id| {
            let mut r = sample_record(id);
            r.status = "suspended".into();
            Ok(r)
        });

        let server = make_server(mock);
        let resp = server
            .update_device_status(Request::new(UpdateDeviceStatusRequest {
                device_id: "dev-1".into(),
                status: "suspended".into(),
                reason: "maintenance".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.device.unwrap().status, "suspended");
    }

    #[tokio::test]
    async fn list_devices_returns_records() {
        let mut mock = MockDeviceStore::new();
        mock.expect_list_devices().returning(|_, _, _, _| {
            Ok((vec![sample_record("d1"), sample_record("d2")], None))
        });

        let server = make_server(mock);
        let resp = server
            .list_devices(Request::new(ListDevicesRequest {
                model: "".into(),
                status: "".into(),
                limit: 10,
                cursor: "".into(),
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(resp.devices.len(), 2);
    }
}

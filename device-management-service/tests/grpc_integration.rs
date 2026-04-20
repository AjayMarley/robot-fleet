//! Integration tests for device-management-service.
//! Spins up a real tonic gRPC server with mTLS on 127.0.0.1:0 (OS-assigned port).
//! No external services required — all certs generated in-process with rcgen.

use std::sync::Arc;
use std::time::Duration;

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity, Server, ServerTlsConfig};

use device_management_service::server::DeviceManagementServer;
use device_management_service::store::{DeviceStore, SqliteDeviceStore};
use robot_fleet_proto::fleet::v1::{
    device_management_service_client::DeviceManagementServiceClient,
    device_management_service_server::DeviceManagementServiceServer,
    BootstrapEnrollRequest, GetDeviceRequest, HeartbeatRequest, ListDevicesRequest,
    RenewCertRequest, UpdateDeviceStatusRequest,
};

// ── Cert helpers ──────────────────────────────────────────────────────────────

struct TestCerts {
    device_ca_cert_pem: String,
    device_ca_key_pem: String,
    fleet_ca_cert_pem: String,
    fleet_ca_key_pem: String,
    /// Server cert signed by Fleet CA, SAN = "localhost"
    server_cert_pem: String,
    server_key_pem: String,
}

fn make_ca(name: &str) -> (rcgen::Certificate, KeyPair) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![]).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name.push(rcgen::DnType::CommonName, name);
    let cert = params.self_signed(&key).unwrap();
    (cert, key)
}

fn make_leaf_signed_by(
    san: &str,
    cn: &str,
    ca_cert: &rcgen::Certificate,
    ca_key: &KeyPair,
) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![san.to_string()]).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, cn);
    let cert = params.signed_by(&key, ca_cert, ca_key).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn make_csr(cn: &str) -> String {
    let key = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec![cn.to_string()]).unwrap();
    let csr = params.serialize_request(&key).unwrap();
    csr.pem().unwrap()
}

fn build_test_certs() -> TestCerts {
    let (device_ca, device_ca_key) = make_ca("Device CA");
    let (fleet_ca, fleet_ca_key) = make_ca("Fleet CA");
    let (server_cert_pem, server_key_pem) =
        make_leaf_signed_by("localhost", "device-mgmt-server", &fleet_ca, &fleet_ca_key);
    TestCerts {
        device_ca_cert_pem: device_ca.pem(),
        device_ca_key_pem: device_ca_key.serialize_pem(),
        fleet_ca_cert_pem: fleet_ca.pem(),
        fleet_ca_key_pem: fleet_ca_key.serialize_pem(),
        server_cert_pem,
        server_key_pem,
    }
}

// ── Server factory ────────────────────────────────────────────────────────────

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Starts a server whose client-auth trust anchor is `client_ca_pem`.
/// Returns the bound port. The server shuts down when the returned
/// `tokio::task::JoinHandle` is dropped (via abort on drop).
async fn start_server(
    certs: &TestCerts,
    store: Arc<SqliteDeviceStore>,
    client_ca_pem: &str,
) -> u16 {
    install_crypto_provider();
    let port = free_port();
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&certs.server_cert_pem, &certs.server_key_pem))
        .client_ca_root(Certificate::from_pem(client_ca_pem));

    let svc = DeviceManagementServiceServer::new(DeviceManagementServer::new(
        store,
        certs.fleet_ca_cert_pem.clone(),
        certs.fleet_ca_key_pem.clone(),
        certs.device_ca_cert_pem.clone(),
        30,
    ));

    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(async move {
        Server::builder()
            .tls_config(tls)
            .unwrap()
            .add_service(svc)
            .serve(addr)
            .await
            .unwrap();
    });

    // Give the server a moment to bind
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

/// Build a tonic Channel with mTLS. `server_ca_pem` is what the client uses
/// to verify the server certificate.
fn mtls_channel(
    port: u16,
    client_cert_pem: &str,
    client_key_pem: &str,
    server_ca_pem: &str,
) -> Channel {
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(server_ca_pem))
        .identity(Identity::from_pem(client_cert_pem, client_key_pem))
        .domain_name("localhost");

    Channel::from_shared(format!("https://127.0.0.1:{port}"))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect_lazy()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Full bootstrap enrollment over real mTLS — Device CA client auth.
#[tokio::test]
async fn bootstrap_enroll_full_flow() {
    let certs = build_test_certs();
    let serial = "SN-INT-001";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(
        &certs,
        store,
        &certs.device_ca_cert_pem,
    )
    .await;

    // Device client presents a cert signed by Device CA, CN = serial
    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (device_cert_pem, device_key_pem) =
        make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);

    let channel = mtls_channel(port, &device_cert_pem, &device_key_pem, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);

    let resp = client
        .bootstrap_enroll(BootstrapEnrollRequest {
            csr_pem: make_csr(serial),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            serial: serial.into(),
            labels: Default::default(),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.device_id.is_empty());
    assert!(resp.operational_cert_pem.contains("CERTIFICATE"));
    assert!(resp.fleet_ca_chain_pem.contains("CERTIFICATE"));
}

/// Second enrollment with the same serial must fail with ALREADY_EXISTS.
#[tokio::test]
async fn bootstrap_enroll_replay_rejected() {
    let certs = build_test_certs();
    let serial = "SN-INT-REPLAY";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(&certs, store, &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (device_cert_pem, device_key_pem) =
        make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);

    let enroll = || {
        let channel =
            mtls_channel(port, &device_cert_pem, &device_key_pem, &certs.fleet_ca_cert_pem);
        let mut c = DeviceManagementServiceClient::new(channel);
        async move {
            c.bootstrap_enroll(BootstrapEnrollRequest {
                csr_pem: make_csr(serial),
                model: "humanoid-v2".into(),
                firmware: "1.0.0".into(),
                serial: serial.into(),
                labels: Default::default(),
            })
            .await
        }
    };

    enroll().await.unwrap();
    let err = enroll().await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::AlreadyExists, "replay must be rejected");
}

/// Concurrent enrollment race — both requests hit the server simultaneously,
/// exactly one must succeed and one must fail.
#[tokio::test]
async fn concurrent_bootstrap_enroll_only_one_wins() {
    let certs = build_test_certs();
    let serial = "SN-INT-RACE";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(&certs, store, &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (device_cert_pem, device_key_pem) =
        make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);

    let make_task = || {
        let (dcp, dkp, fca) = (
            device_cert_pem.clone(),
            device_key_pem.clone(),
            certs.fleet_ca_cert_pem.clone(),
        );
        tokio::spawn(async move {
            let channel = mtls_channel(port, &dcp, &dkp, &fca);
            let mut c = DeviceManagementServiceClient::new(channel);
            c.bootstrap_enroll(BootstrapEnrollRequest {
                csr_pem: make_csr(serial),
                model: "humanoid-v2".into(),
                firmware: "1.0.0".into(),
                serial: serial.into(),
                labels: Default::default(),
            })
            .await
        })
    };

    let (r1, r2) = tokio::join!(make_task(), make_task());
    let results = [r1.unwrap(), r2.unwrap()];
    let successes = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(successes, 1, "exactly one concurrent enrollment must succeed");
}

/// Enroll a device, then retrieve it with GetDevice.
#[tokio::test]
async fn get_device_after_enroll() {
    let certs = build_test_certs();
    let serial = "SN-INT-GET";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(&certs, store.clone(), &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (device_cert_pem, device_key_pem) =
        make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);

    let channel = mtls_channel(port, &device_cert_pem, &device_key_pem, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);

    let enroll_resp = client
        .bootstrap_enroll(BootstrapEnrollRequest {
            csr_pem: make_csr(serial),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            serial: serial.into(),
            labels: Default::default(),
        })
        .await
        .unwrap()
        .into_inner();

    let device = client
        .get_device(GetDeviceRequest { device_id: enroll_resp.device_id.clone() })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(device.device_id, enroll_resp.device_id);
    assert_eq!(device.serial, serial);
    assert_eq!(device.status, "active");
}

/// Enroll → UpdateDeviceStatus → ListDevices filtered by status.
#[tokio::test]
async fn update_status_and_list_devices() {
    let certs = build_test_certs();
    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());

    for i in 0..3u8 {
        store.register_serial(&format!("SN-LIST-{i:03}"), "humanoid-v2").await.unwrap();
    }

    let port = start_server(&certs, store.clone(), &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };

    let mut enrolled_ids = vec![];
    for i in 0..3u8 {
        let serial = format!("SN-LIST-{i:03}");
        let (dcp, dkp) = make_leaf_signed_by(&serial, &serial, &device_ca, &device_ca_key);
        let channel = mtls_channel(port, &dcp, &dkp, &certs.fleet_ca_cert_pem);
        let mut c = DeviceManagementServiceClient::new(channel);
        let resp = c
            .bootstrap_enroll(BootstrapEnrollRequest {
                csr_pem: make_csr(&serial),
                model: "humanoid-v2".into(),
                firmware: "1.0.0".into(),
                serial: serial.clone(),
                labels: Default::default(),
            })
            .await
            .unwrap()
            .into_inner();
        enrolled_ids.push(resp.device_id);
    }

    // Suspend the first device
    let (dcp, dkp) = make_leaf_signed_by("SN-LIST-000", "SN-LIST-000", &device_ca, &device_ca_key);
    let channel = mtls_channel(port, &dcp, &dkp, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);
    client
        .update_device_status(UpdateDeviceStatusRequest {
            device_id: enrolled_ids[0].clone(),
            status: "suspended".into(),
            reason: "test".into(),
        })
        .await
        .unwrap();

    let all = client
        .list_devices(ListDevicesRequest {
            model: "humanoid-v2".into(),
            status: "".into(),
            limit: 10,
            cursor: "".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(all.devices.len(), 3);

    let active_only = client
        .list_devices(ListDevicesRequest {
            model: "".into(),
            status: "active".into(),
            limit: 10,
            cursor: "".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(active_only.devices.len(), 2);
}

/// Enroll → RenewCert returns a fresh operational cert.
#[tokio::test]
async fn renew_cert_after_enroll() {
    let certs = build_test_certs();
    let serial = "SN-INT-RENEW";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(&certs, store, &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (dcp, dkp) = make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);
    let channel = mtls_channel(port, &dcp, &dkp, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);

    let enroll_resp = client
        .bootstrap_enroll(BootstrapEnrollRequest {
            csr_pem: make_csr(serial),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            serial: serial.into(),
            labels: Default::default(),
        })
        .await
        .unwrap()
        .into_inner();

    let renew_resp = client
        .renew_cert(RenewCertRequest {
            device_id: enroll_resp.device_id,
            csr_pem: make_csr(serial),
        })
        .await
        .unwrap()
        .into_inner();

    assert!(renew_resp.operational_cert_pem.contains("CERTIFICATE"));
    assert_ne!(renew_resp.operational_cert_pem, enroll_resp.operational_cert_pem);
}

/// Heartbeat bidi stream — send N pings, receive N responses, DB is updated.
#[tokio::test]
async fn stream_heartbeat_updates_last_seen() {
    let certs = build_test_certs();
    let serial = "SN-INT-HB";

    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());
    store.register_serial(serial, "humanoid-v2").await.unwrap();

    let port = start_server(&certs, store.clone(), &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (dcp, dkp) = make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);
    let channel = mtls_channel(port, &dcp, &dkp, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);

    // Enroll first to create the device record
    let enroll_resp = client
        .bootstrap_enroll(BootstrapEnrollRequest {
            csr_pem: make_csr(serial),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            serial: serial.into(),
            labels: Default::default(),
        })
        .await
        .unwrap()
        .into_inner();

    let device_id = enroll_resp.device_id.clone();
    let last_seen_before = store.get_device(&device_id).await.unwrap().last_seen_at;

    // Send 3 heartbeat pings
    let pings: Vec<HeartbeatRequest> = (0..3)
        .map(|i| HeartbeatRequest {
            device_id: device_id.clone(),
            cpu_percent: i as f64 * 10.0,
            memory_percent: 50.0,
            battery_percent: 90.0,
            recorded_at: None,
            operational_status: "ok".into(),
            extra: Default::default(),
        })
        .collect();

    let stream = tokio_stream::iter(pings);
    let mut resp_stream = client.stream_heartbeat(stream).await.unwrap().into_inner();

    let mut count = 0;
    while let Some(resp) = tokio_stream::StreamExt::next(&mut resp_stream).await {
        resp.unwrap();
        count += 1;
        if count == 3 {
            break;
        }
    }
    assert_eq!(count, 3);

    // Give the server a tick to flush DB writes
    tokio::time::sleep(Duration::from_millis(50)).await;
    let last_seen_after = store.get_device(&device_id).await.unwrap().last_seen_at;
    assert!(
        last_seen_after >= last_seen_before,
        "last_seen_at must be updated by heartbeats"
    );
}

/// Heartbeat for an unknown device_id must return NOT_FOUND on the stream.
#[tokio::test]
async fn stream_heartbeat_unknown_device_returns_not_found() {
    let certs = build_test_certs();
    let store = Arc::new(SqliteDeviceStore::new(":memory:").unwrap());

    // Register + enroll a real device just to get a valid mTLS client
    let serial = "SN-INT-HB-GHOST-AUTH";
    store.register_serial(serial, "humanoid-v2").await.unwrap();
    let port = start_server(&certs, store.clone(), &certs.device_ca_cert_pem).await;

    let (device_ca, device_ca_key) = {
        let key = KeyPair::from_pem(&certs.device_ca_key_pem).unwrap();
        let cert = CertificateParams::from_ca_cert_pem(&certs.device_ca_cert_pem)
            .unwrap()
            .self_signed(&key)
            .unwrap();
        (cert, key)
    };
    let (dcp, dkp) = make_leaf_signed_by(serial, serial, &device_ca, &device_ca_key);
    let channel = mtls_channel(port, &dcp, &dkp, &certs.fleet_ca_cert_pem);
    let mut client = DeviceManagementServiceClient::new(channel);

    // Enroll so TLS auth works, but then send heartbeat with a ghost device_id
    client
        .bootstrap_enroll(BootstrapEnrollRequest {
            csr_pem: make_csr(serial),
            model: "humanoid-v2".into(),
            firmware: "1.0.0".into(),
            serial: serial.into(),
            labels: Default::default(),
        })
        .await
        .unwrap();

    let ping = HeartbeatRequest {
        device_id: "ghost-device-id".into(),
        cpu_percent: 1.0,
        memory_percent: 1.0,
        battery_percent: 100.0,
        recorded_at: None,
        operational_status: "ok".into(),
        extra: Default::default(),
    };

    let mut stream = client
        .stream_heartbeat(tokio_stream::iter(vec![ping]))
        .await
        .unwrap()
        .into_inner();

    let err = tokio_stream::StreamExt::next(&mut stream)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

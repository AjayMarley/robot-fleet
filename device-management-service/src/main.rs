use std::fs;
use std::sync::Arc;

use anyhow::Context;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;

use robot_fleet_proto::fleet::v1::device_management_service_server::DeviceManagementServiceServer;
use robot_fleet_proto::fleet::v1::provisioning_service_server::ProvisioningServiceServer;
use robot_fleet_proto::fleet::v1::telemetry_service_server::TelemetryServiceServer;

use device_management_service::provisioning::ProvisioningServer;
use device_management_service::server::DeviceManagementServer;
use device_management_service::store::{DeviceStore, SqliteDeviceStore};
use device_management_service::telemetry::TelemetryServer;

const HEALTH_TICK_SECS: u64 = 30;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt::init();

    let listen_addr       = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "[::]:8443".into());
    let bootstrap_addr    = std::env::var("BOOTSTRAP_ADDR").unwrap_or_else(|_| "[::]:8444".into());
    let provisioning_addr = std::env::var("PROVISIONING_ADDR").unwrap_or_else(|_| "[::]:8445".into());
    let db_path           = std::env::var("DB_PATH").unwrap_or_else(|_| "./fleet.db".into());
    let validity_days: u32 = std::env::var("CERT_VALIDITY_DAYS")
        .unwrap_or_else(|_| "30".into())
        .parse()
        .context("CERT_VALIDITY_DAYS must be a number")?;

    let server_cert    = fs::read(std::env::var("SERVER_CERT_PEM").context("SERVER_CERT_PEM")?)?;
    let server_key     = fs::read(std::env::var("SERVER_KEY_PEM").context("SERVER_KEY_PEM")?)?;
    let fleet_ca_cert  = fs::read(std::env::var("FLEET_CA_CERT_PEM").context("FLEET_CA_CERT_PEM")?)?;
    let fleet_ca_key   = fs::read(std::env::var("FLEET_CA_KEY_PEM").context("FLEET_CA_KEY_PEM")?)?;
    let device_ca_cert = fs::read(std::env::var("DEVICE_CA_CERT_PEM").context("DEVICE_CA_CERT_PEM")?)?;
    let device_ca_key  = fs::read(std::env::var("DEVICE_CA_KEY_PEM").context("DEVICE_CA_KEY_PEM")?)?;

    info!(
        db = %db_path,
        fleet_addr = %listen_addr,
        bootstrap_addr = %bootstrap_addr,
        provisioning_addr = %provisioning_addr,
        cert_validity_days = validity_days,
        "device-management-service starting"
    );

    let store = Arc::new(SqliteDeviceStore::new(&db_path).context("failed to open DB")?);

    // ── Phase 0: seed factory manifest ───────────────────────────────────────
    // Format: "serial:token:model,serial:token:model,..."  model is optional (defaults to Alpha Wheeled)
    let mut manifest_count = 0u32;
    if let Ok(manifest) = std::env::var("FACTORY_MANIFEST") {
        for entry in manifest.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let parts: Vec<&str> = entry.splitn(3, ':').collect();
            let (serial, token, model) = match parts.as_slice() {
                [s, t, m] => (*s, *t, *m),
                [s, t]    => (*s, *t, "Alpha Wheeled"),
                _         => { tracing::warn!("invalid FACTORY_MANIFEST entry: {entry}"); continue; }
            };
            store.seed_manifest(serial, model, token).await
                .unwrap_or_else(|e| tracing::warn!("manifest seed {serial}: {e}"));
            info!(serial, model, "[Phase 0] factory manifest seeded — manufacturing token registered");
            manifest_count += 1;
        }
    }

    let enrolled = store.count_devices().await.unwrap_or(0);
    info!(manifest_entries = manifest_count, already_enrolled = enrolled, "store ready");

    let fleet_ca_cert_pem  = String::from_utf8(fleet_ca_cert).context("FLEET_CA_CERT_PEM not UTF-8")?;
    let fleet_ca_key_pem   = String::from_utf8(fleet_ca_key).context("FLEET_CA_KEY_PEM not UTF-8")?;
    let device_ca_cert_pem = String::from_utf8(device_ca_cert).context("DEVICE_CA_CERT_PEM not UTF-8")?;
    let device_ca_key_pem  = String::from_utf8(device_ca_key).context("DEVICE_CA_KEY_PEM not UTF-8")?;

    // ── Port 8443: fleet operations (mTLS — Fleet CA client auth) ────────────
    let fleet_tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key))
        .client_ca_root(Certificate::from_pem(fleet_ca_cert_pem.as_bytes()));

    let health_store = store.clone();

    let fleet_svc = DeviceManagementServiceServer::new(DeviceManagementServer::new(
        store.clone(),
        fleet_ca_cert_pem.clone(),
        fleet_ca_key_pem.clone(),
        device_ca_cert_pem.clone(),
        validity_days,
    ));

    // ── Port 8444: bootstrap enrollment (mTLS — Device CA client auth) ───────
    let bootstrap_tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key))
        .client_ca_root(Certificate::from_pem(device_ca_cert_pem.as_bytes()));

    let bootstrap_svc = DeviceManagementServiceServer::new(DeviceManagementServer::new(
        store.clone(),
        fleet_ca_cert_pem,
        fleet_ca_key_pem,
        device_ca_cert_pem.clone(),
        validity_days,
    ));

    // ── Port 8445: provisioning (TLS only — no client cert, token auth) ──────
    let provisioning_tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key));

    let provisioning_svc = ProvisioningServiceServer::new(ProvisioningServer::new(
        store.clone(),
        device_ca_cert_pem,
        device_ca_key_pem,
    ));

    let fleet_addr        = listen_addr.parse().context("invalid LISTEN_ADDR")?;
    let bootstrap_addr    = bootstrap_addr.parse().context("invalid BOOTSTRAP_ADDR")?;
    let provisioning_addr = provisioning_addr.parse().context("invalid PROVISIONING_ADDR")?;

    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!(addr = %provisioning_addr, "port 8445 OPEN  [Phase 1 — factory provisioning]   TLS-only, token auth, Device CA signing");
    info!(addr = %bootstrap_addr,    "port 8444 OPEN  [Phase 2 — bootstrap enrollment]   mTLS (Device CA), Fleet CA signing");
    info!(addr = %fleet_addr,        "port 8443 OPEN  [Phase 3 — normal operations]       mTLS (Fleet CA), heartbeat/telemetry/OTA");
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            std::time::Duration::from_secs(HEALTH_TICK_SECS)
        );
        interval.tick().await;
        loop {
            interval.tick().await;
            match health_store.count_devices().await {
                Ok(n) => info!(enrolled_devices = n, "[health tick] active fleet size"),
                Err(e) => tracing::warn!("health tick error: {e}"),
            }
        }
    });

    tokio::try_join!(
        Server::builder()
            .tls_config(fleet_tls)
            .context("fleet tls_config")?
            .add_service(fleet_svc)
            .add_service(TelemetryServiceServer::new(TelemetryServer))
            .serve(fleet_addr),
        Server::builder()
            .tls_config(bootstrap_tls)
            .context("bootstrap tls_config")?
            .add_service(bootstrap_svc)
            .serve(bootstrap_addr),
        Server::builder()
            .tls_config(provisioning_tls)
            .context("provisioning tls_config")?
            .add_service(provisioning_svc)
            .serve(provisioning_addr),
    )
    .context("server error")?;

    Ok(())
}

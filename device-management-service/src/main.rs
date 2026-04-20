use std::fs;
use std::sync::Arc;

use anyhow::Context;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;

use robot_fleet_proto::fleet::v1::device_management_service_server::DeviceManagementServiceServer;

use device_management_service::server::DeviceManagementServer;
use device_management_service::store::SqliteDeviceStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt::init();

    let listen_addr    = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "[::]:8443".into());
    let bootstrap_addr = std::env::var("BOOTSTRAP_ADDR").unwrap_or_else(|_| "[::]:8444".into());
    let db_path        = std::env::var("DB_PATH").unwrap_or_else(|_| "./fleet.db".into());
    let validity_days: u32 = std::env::var("CERT_VALIDITY_DAYS")
        .unwrap_or_else(|_| "30".into())
        .parse()
        .context("CERT_VALIDITY_DAYS must be a number")?;

    let server_cert       = fs::read(std::env::var("SERVER_CERT_PEM").context("SERVER_CERT_PEM")?)?;
    let server_key        = fs::read(std::env::var("SERVER_KEY_PEM").context("SERVER_KEY_PEM")?)?;
    let fleet_ca_cert     = fs::read(std::env::var("FLEET_CA_CERT_PEM").context("FLEET_CA_CERT_PEM")?)?;
    let fleet_ca_key      = fs::read(std::env::var("FLEET_CA_KEY_PEM").context("FLEET_CA_KEY_PEM")?)?;
    let device_ca_cert    = fs::read(std::env::var("DEVICE_CA_CERT_PEM").context("DEVICE_CA_CERT_PEM")?)?;

    let store = Arc::new(SqliteDeviceStore::new(&db_path).context("failed to open DB")?);

    // Operational port (Fleet CA client auth) — all RPCs except BootstrapEnroll
    let fleet_tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key))
        .client_ca_root(Certificate::from_pem(&fleet_ca_cert));

    let fleet_svc = DeviceManagementServiceServer::new(DeviceManagementServer::new(
        store.clone(),
        String::from_utf8(fleet_ca_cert.clone()).context("FLEET_CA_CERT_PEM not UTF-8")?,
        String::from_utf8(fleet_ca_key).context("FLEET_CA_KEY_PEM not UTF-8")?,
        String::from_utf8(device_ca_cert.clone()).context("DEVICE_CA_CERT_PEM not UTF-8")?,
        validity_days,
    ));

    // Bootstrap port (Device CA client auth) — BootstrapEnroll only
    let bootstrap_tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key))
        .client_ca_root(Certificate::from_pem(&device_ca_cert));

    let bootstrap_svc = DeviceManagementServiceServer::new(DeviceManagementServer::new(
        store,
        String::from_utf8(fleet_ca_cert).context("FLEET_CA_CERT_PEM not UTF-8")?,
        String::new(), // key not needed for bootstrap port (no cert signing here)
        String::from_utf8(device_ca_cert).context("DEVICE_CA_CERT_PEM not UTF-8")?,
        validity_days,
    ));

    let fleet_addr     = listen_addr.parse().context("invalid LISTEN_ADDR")?;
    let bootstrap_addr = bootstrap_addr.parse().context("invalid BOOTSTRAP_ADDR")?;

    info!("fleet port listening on {fleet_addr}");
    info!("bootstrap port listening on {bootstrap_addr}");

    tokio::try_join!(
        Server::builder()
            .tls_config(fleet_tls)
            .context("fleet tls_config")?
            .add_service(fleet_svc)
            .serve(fleet_addr),
        Server::builder()
            .tls_config(bootstrap_tls)
            .context("bootstrap tls_config")?
            .add_service(bootstrap_svc)
            .serve(bootstrap_addr),
    )
    .context("server error")?;

    Ok(())
}

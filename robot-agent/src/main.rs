use std::path::PathBuf;

use anyhow::Context;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tracing::info;

mod enrollment;
mod error;
mod heartbeat;
mod ota;
mod telemetry;

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("missing env var {key}"))
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn mtls_config(cert_pem: &str, key_pem: &str, ca_pem: &str) -> ClientTlsConfig {
    ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca_pem))
        .identity(Identity::from_pem(cert_pem, key_pem))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cert_dir = PathBuf::from(env_or("AGENT_DATA_DIR", "/root/.robot-agent"));
    std::fs::create_dir_all(&cert_dir)?;

    let manager = enrollment::EnrollmentManager {
        cert_dir: cert_dir.clone(),
        device_cert_pem: std::fs::read_to_string(env_required("DEVICE_CERT_PEM")?)?,
        device_key_pem: std::fs::read_to_string(env_required("DEVICE_KEY_PEM")?)?,
        device_ca_pem: std::fs::read_to_string(env_required("DEVICE_CA_CERT_PEM")?)?,
        serial: env_required("DEVICE_SERIAL")?,
        model: env_or("DEVICE_MODEL", "robot-v1"),
        firmware: env_or("FIRMWARE_VERSION", "0.1.0"),
    };

    // Bootstrap enrollment — device cert as client cert, fleet CA to verify server
    // The server cert is signed by fleet-ca; device-ca is only used server-side
    // to verify our client cert.
    let fleet_ca_pem = std::fs::read_to_string(env_required("FLEET_CA_CERT_PEM")?)?;
    if !manager.is_enrolled() {
        info!("Not enrolled — starting bootstrap enrollment");
        let bootstrap_addr = env_required("BOOTSTRAP_ADDR")?;
        let bootstrap_tls = mtls_config(&manager.device_cert_pem, &manager.device_key_pem, &fleet_ca_pem);
        let bootstrap_channel = Channel::from_shared(bootstrap_addr)?
            .tls_config(bootstrap_tls)?
            .connect()
            .await
            .context("connect to bootstrap endpoint")?;
        manager.enroll(bootstrap_channel).await?;
    }

    info!("Loading operational credentials");
    let (op_cert, op_key) = manager.load_operational_creds()?;
    let fleet_ca = fleet_ca_pem;
    let device_id = env_or("DEVICE_SERIAL", &manager.serial);

    // Operational channels use Fleet CA mTLS
    let fleet_addr = env_required("FLEET_SERVICE_ADDR")?;
    let ota_addr = env_or("OTA_SERVICE_ADDR", &fleet_addr);

    let fleet_channel = Channel::from_shared(fleet_addr)?
        .tls_config(mtls_config(&op_cert, &op_key, &fleet_ca))?
        .connect()
        .await
        .context("connect to fleet service")?;

    let ota_channel = Channel::from_shared(ota_addr)?
        .tls_config(mtls_config(&op_cert, &op_key, &fleet_ca))?
        .connect()
        .await
        .context("connect to OTA service")?;

    let socket_path = std::env::var("SOCKET_PATH").ok().map(PathBuf::from);

    info!(device_id = %device_id, "Starting agent loops");

    let heartbeat = tokio::spawn(heartbeat::run(device_id.clone(), fleet_channel.clone()));
    let ota = tokio::spawn(ota::run(device_id.clone(), ota_channel));
    let telemetry = tokio::spawn(telemetry::run(device_id.clone(), fleet_channel, socket_path));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("Ctrl-C — shutting down"),
        res = heartbeat => { if let Ok(Err(e)) = res { tracing::error!("Heartbeat: {e}") } },
        res = ota =>       { if let Ok(Err(e)) = res { tracing::error!("OTA: {e}") } },
        res = telemetry => { if let Ok(Err(e)) = res { tracing::error!("Telemetry: {e}") } },
    }

    info!("robot-agent exiting");
    Ok(())
}

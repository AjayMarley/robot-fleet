use std::path::PathBuf;

use anyhow::Context;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tracing::info;

mod enrollment;
mod error;
mod heartbeat;
mod ota;
mod provisioning;
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

    let serial   = env_required("DEVICE_SERIAL")?;
    let fleet_ca_pem = std::fs::read_to_string(env_required("FLEET_CA_CERT_PEM")?)?;

    let manager = enrollment::EnrollmentManager {
        cert_dir: cert_dir.clone(),
        serial:   serial.clone(),
        model:    env_or("DEVICE_MODEL", "h1-humanoid"),
        firmware: env_or("FIRMWARE_VERSION", "0.1.0"),
    };

    // ── Phase 1: provisioning ─────────────────────────────────────────────────
    // On factory-fresh device: no device cert exists. Contact the provisioning
    // service with the one-time token to get a Device-CA-signed cert.
    if !manager.is_provisioned() {
        let token            = env_required("PROVISION_TOKEN")?;
        let provisioning_addr = env_required("PROVISIONING_ADDR")?;
        provisioning::provision(
            &serial,
            &token,
            &provisioning_addr,
            &fleet_ca_pem,
            &cert_dir,
        )
        .await
        .context("Phase 1 provisioning failed")?;
    }

    // ── Phase 2: bootstrap enrollment ────────────────────────────────────────
    // Device cert now exists. Connect with mTLS (device cert / Device CA trust)
    // and exchange for a Fleet-CA-signed operational cert.
    if !manager.is_enrolled() {
        info!(serial = %serial, "Phase 2 — not enrolled, starting bootstrap");
        let (device_cert_pem, device_key_pem) = manager.load_device_cert()?;
        let bootstrap_addr = env_required("BOOTSTRAP_ADDR")?;
        let bootstrap_channel = Channel::from_shared(bootstrap_addr)?
            .tls_config(mtls_config(&device_cert_pem, &device_key_pem, &fleet_ca_pem))?
            .connect()
            .await
            .context("connect to bootstrap endpoint")?;
        manager.enroll(bootstrap_channel).await?;
    }

    // ── Phase 3: normal operation ─────────────────────────────────────────────
    info!(port = 8443, "[Phase 3] loading operational credentials — connecting with Fleet CA mTLS");
    let (op_cert, op_key) = manager.load_operational_creds()?;
    let device_id = manager.load_device_id()?;

    let fleet_addr = env_required("FLEET_SERVICE_ADDR")?;
    let ota_addr   = env_or("OTA_SERVICE_ADDR", &fleet_addr);

    let fleet_channel = Channel::from_shared(fleet_addr)?
        .tls_config(mtls_config(&op_cert, &op_key, &fleet_ca_pem))?
        .connect()
        .await
        .context("connect to fleet service")?;

    let ota_channel = Channel::from_shared(ota_addr)?
        .tls_config(mtls_config(&op_cert, &op_key, &fleet_ca_pem))?
        .connect()
        .await
        .context("connect to OTA service")?;

    let socket_path = std::env::var("SOCKET_PATH").ok().map(PathBuf::from);

    info!(device_id = %device_id, port = 8443, "[Phase 3] agent online — heartbeat + telemetry + OTA watch running");

    let heartbeat = tokio::spawn(heartbeat::run(device_id.clone(), fleet_channel.clone()));
    let ota       = tokio::spawn(ota::run(device_id.clone(), ota_channel));
    let telemetry = tokio::spawn(telemetry::run(device_id.clone(), fleet_channel, socket_path));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("Ctrl-C — shutting down"),
        res = heartbeat => { if let Ok(Err(e)) = res { tracing::error!("Heartbeat: {e}") } },
        res = ota       => { if let Ok(Err(e)) = res { tracing::error!("OTA: {e}") } },
        res = telemetry => { if let Ok(Err(e)) = res { tracing::error!("Telemetry: {e}") } },
    }

    info!("robot-agent exiting");
    Ok(())
}

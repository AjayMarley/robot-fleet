use std::sync::Arc;

use anyhow::Context;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;

mod dispatcher;
mod error;
mod server;
mod store;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key).with_context(|| format!("missing env var {key}"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let listen_addr = env_or("LISTEN_ADDR", "[::]:9444").parse()?;
    let db_path = env_or("DB_PATH", "./ota.db");

    let cert_pem = std::fs::read_to_string(env_required("SERVER_CERT_PEM")?)?;
    let key_pem = std::fs::read_to_string(env_required("SERVER_KEY_PEM")?)?;
    let ca_pem = std::fs::read_to_string(env_required("FLEET_CA_CERT_PEM")?)?;

    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&cert_pem, &key_pem))
        .client_ca_root(Certificate::from_pem(&ca_pem));

    let store = Arc::new(store::SqliteJobStore::new(&db_path)?);
    let dispatcher = dispatcher::Dispatcher::new();

    let svc = robot_fleet_proto::fleet::v1::ota_service_server::OtaServiceServer::new(
        server::OtaServer { store, dispatcher },
    );

    info!(%listen_addr, "ota-service starting");
    Server::builder()
        .tls_config(tls)?
        .add_service(svc)
        .serve(listen_addr)
        .await?;

    Ok(())
}

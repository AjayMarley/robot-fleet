use std::fs;
use std::sync::Arc;

use anyhow::Context;
use robot_fleet_proto::artifacts::v1::artifact_service_server::ArtifactServiceServer;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;

use artifact_store::registry::SqliteRegistry;
use artifact_store::server::ArtifactServer;
use artifact_store::storage::MinioStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let listen_addr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "[::]:9443".into());
    let db_path     = std::env::var("DB_PATH").unwrap_or_else(|_| "./artifacts.db".into());

    let server_cert = fs::read(std::env::var("SERVER_CERT_PEM").context("SERVER_CERT_PEM")?)?;
    let server_key  = fs::read(std::env::var("SERVER_KEY_PEM").context("SERVER_KEY_PEM")?)?;
    let fleet_ca    = fs::read(std::env::var("FLEET_CA_CERT_PEM").context("FLEET_CA_CERT_PEM")?)?;

    let minio_endpoint   = std::env::var("MINIO_ENDPOINT").unwrap_or_else(|_| "http://localhost:9000".into());
    let minio_access_key = std::env::var("MINIO_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let minio_secret_key = std::env::var("MINIO_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());
    let minio_bucket     = std::env::var("MINIO_BUCKET").unwrap_or_else(|_| "robot-artifacts".into());

    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&server_cert, &server_key))
        .client_ca_root(Certificate::from_pem(&fleet_ca));

    let registry = Arc::new(SqliteRegistry::new(&db_path).context("failed to open DB")?);
    let storage  = Arc::new(MinioStore::new(&minio_endpoint, &minio_access_key, &minio_secret_key, &minio_bucket));
    let svc      = ArtifactServiceServer::new(ArtifactServer::new(registry, storage));

    let addr = listen_addr.parse().context("invalid LISTEN_ADDR")?;
    info!("artifact-store listening on {addr}");

    Server::builder()
        .tls_config(tls)
        .context("tls_config")?
        .add_service(svc)
        .serve(addr)
        .await
        .context("server error")
}

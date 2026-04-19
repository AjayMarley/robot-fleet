use std::sync::Arc;

use tonic::{Request, Response, Status};

use robot_fleet_proto::artifacts::v1::{
    artifact_service_server::ArtifactService, ArtifactMeta, GetArtifactRequest, GetLatestRequest,
    ListArtifactsRequest, ListArtifactsResponse, PublishRequest, PublishResponse,
    UploadUrlRequest, UploadUrlResponse,
};

use crate::error::Error;
use crate::registry::{ArtifactRecord, NewArtifact, Registry};
use crate::storage::ObjectStore;

// ── Server ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ArtifactServer<R, S> {
    registry: Arc<R>,
    storage: Arc<S>,
}

impl<R, S> ArtifactServer<R, S> {
    pub fn new(registry: Arc<R>, storage: Arc<S>) -> Self {
        Self { registry, storage }
    }
}

fn record_to_proto(r: ArtifactRecord) -> ArtifactMeta {
    ArtifactMeta {
        artifact_id: r.id,
        model: r.model,
        version: r.version,
        download_url: r.download_url,
        sha256: r.sha256,
        size_bytes: r.size_bytes as u64,
        channel: r.channel,
        created_at: Some(prost_types::Timestamp {
            seconds: r.created_at,
            nanos: 0,
        }),
        metadata: Default::default(),
    }
}

fn map_err(e: Error) -> Status {
    match e {
        Error::NotFound(msg) => Status::not_found(msg),
        Error::VersionConflict { .. } => Status::already_exists(e.to_string()),
        _ => Status::internal(e.to_string()),
    }
}

#[tonic::async_trait]
impl<R, S> ArtifactService for ArtifactServer<R, S>
where
    R: Registry + Send + Sync + 'static,
    S: ObjectStore + Send + Sync + 'static,
{
    async fn publish_artifact(
        &self,
        request: Request<PublishRequest>,
    ) -> Result<Response<PublishResponse>, Status> {
        let req = request.into_inner();
        if req.sha256.is_empty() {
            return Err(Status::invalid_argument("sha256 cannot be empty"));
        }
        let record = self
            .registry
            .create(NewArtifact {
                model: req.model,
                version: req.version,
                sha256: req.sha256,
                size_bytes: req.size_bytes as i64,
                channel: req.channel,
                object_key: req.object_key,
                download_url: String::new(),
            })
            .await
            .map_err(map_err)?;
        Ok(Response::new(PublishResponse {
            artifact: Some(record_to_proto(record)),
        }))
    }

    async fn get_artifact(
        &self,
        request: Request<GetArtifactRequest>,
    ) -> Result<Response<ArtifactMeta>, Status> {
        let record = self
            .registry
            .get(&request.into_inner().artifact_id)
            .await
            .map_err(map_err)?;
        Ok(Response::new(record_to_proto(record)))
    }

    async fn list_artifacts(
        &self,
        request: Request<ListArtifactsRequest>,
    ) -> Result<Response<ListArtifactsResponse>, Status> {
        let req = request.into_inner();
        let limit = if req.limit > 0 { req.limit as i64 } else { 50 };
        let records = self
            .registry
            .list(&req.model, &req.channel, limit)
            .await
            .map_err(map_err)?;
        Ok(Response::new(ListArtifactsResponse {
            artifacts: records.into_iter().map(record_to_proto).collect(),
        }))
    }

    async fn get_latest(
        &self,
        request: Request<GetLatestRequest>,
    ) -> Result<Response<ArtifactMeta>, Status> {
        let req = request.into_inner();
        let record = self
            .registry
            .get_latest(&req.model, &req.channel)
            .await
            .map_err(map_err)?;
        Ok(Response::new(record_to_proto(record)))
    }

    async fn generate_upload_url(
        &self,
        request: Request<UploadUrlRequest>,
    ) -> Result<Response<UploadUrlResponse>, Status> {
        let req = request.into_inner();
        let key = format!("{}/{}/firmware.bin", req.model, req.version);
        let upload_url = self
            .storage
            .presign_upload(&key, 3600)
            .await
            .map_err(map_err)?;
        Ok(Response::new(UploadUrlResponse { upload_url, object_key: key }))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::SqliteRegistry;
    use crate::storage::MockObjectStore;

    fn make_server() -> ArtifactServer<SqliteRegistry, MockObjectStore> {
        let registry = SqliteRegistry::new(":memory:").unwrap();
        let storage = MockObjectStore::new();
        ArtifactServer::new(Arc::new(registry), Arc::new(storage))
    }

    #[tokio::test]
    async fn publish_returns_artifact_meta() {
        let server = make_server();
        let resp = server
            .publish_artifact(Request::new(PublishRequest {
                model: "humanoid-v2".into(),
                version: "1.0.0".into(),
                sha256: "deadbeef".into(),
                size_bytes: 2048,
                channel: "stable".into(),
                object_key: "humanoid-v2/1.0.0/fw.bin".into(),
                metadata: Default::default(),
            }))
            .await
            .unwrap();
        let meta = resp.into_inner().artifact.unwrap();
        assert_eq!(meta.version, "1.0.0");
        assert!(!meta.artifact_id.is_empty());
    }

    #[tokio::test]
    async fn publish_rejects_empty_sha256() {
        let server = make_server();
        let err = server
            .publish_artifact(Request::new(PublishRequest {
                model: "humanoid-v2".into(),
                version: "1.0.0".into(),
                sha256: String::new(),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn generate_upload_url_calls_storage() {
        let registry = SqliteRegistry::new(":memory:").unwrap();
        let mut storage = MockObjectStore::new();
        storage
            .expect_presign_upload()
            .returning(|_, _| Ok("https://minio.example.com/presigned".into()));

        let server = ArtifactServer::new(Arc::new(registry), Arc::new(storage));
        let resp = server
            .generate_upload_url(Request::new(UploadUrlRequest {
                model: "humanoid-v2".into(),
                version: "2.0.0".into(),
                content_type: "application/octet-stream".into(),
            }))
            .await
            .unwrap();
        assert!(resp.into_inner().upload_url.contains("presigned"));
    }
}

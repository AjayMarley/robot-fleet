//! Integration tests against a real MinIO instance.
//! Run with: cargo test -p artifact-store --features integration --test minio_integration
//!
//! Requires MinIO on localhost:9000 (minioadmin/minioadmin).

#![cfg(feature = "integration")]

use std::sync::Arc;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::primitives::ByteStream;
use tonic::Request;

use artifact_store::registry::SqliteRegistry;
use artifact_store::server::ArtifactServer;
use artifact_store::storage::{MinioStore, ObjectStore};
use robot_fleet_proto::artifacts::v1::{
    artifact_service_server::ArtifactService, GetArtifactRequest, GetLatestRequest,
    ListArtifactsRequest, PublishRequest, UploadUrlRequest,
};

const ENDPOINT: &str = "http://localhost:9000";
const ACCESS: &str = "minioadmin";
const SECRET: &str = "minioadmin";
const BUCKET: &str = "test-artifacts";

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn setup() -> MinioStore {
    let creds = Credentials::new(ACCESS, SECRET, None, None, "static");
    let cfg = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .endpoint_url(ENDPOINT)
        .credentials_provider(creds)
        .region(Region::new("us-east-1"))
        .force_path_style(true)
        .build();
    let client = aws_sdk_s3::Client::from_conf(cfg);
    let _ = client.create_bucket().bucket(BUCKET).send().await;
    MinioStore::new(ENDPOINT, ACCESS, SECRET, BUCKET)
}

async fn raw_client() -> aws_sdk_s3::Client {
    let creds = Credentials::new(ACCESS, SECRET, None, None, "static");
    let cfg = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .endpoint_url(ENDPOINT)
        .credentials_provider(creds)
        .region(Region::new("us-east-1"))
        .force_path_style(true)
        .build();
    aws_sdk_s3::Client::from_conf(cfg)
}

fn make_server() -> ArtifactServer<SqliteRegistry, MinioStore> {
    let registry = SqliteRegistry::new(":memory:").unwrap();
    let storage = MinioStore::new(ENDPOINT, ACCESS, SECRET, BUCKET);
    ArtifactServer::new(Arc::new(registry), Arc::new(storage))
}

// ── ObjectStore trait tests ───────────────────────────────────────────────────

#[tokio::test]
async fn object_exists_returns_false_for_missing_key() {
    let store = setup().await;
    let exists = store.object_exists("does/not/exist.bin").await.unwrap();
    assert!(!exists);
}

#[tokio::test]
async fn object_exists_returns_true_after_upload() {
    let store = setup().await;
    let key = "integration/test-object.bin";

    let client = raw_client().await;
    client
        .put_object()
        .bucket(BUCKET)
        .key(key)
        .body(ByteStream::from(b"hello".to_vec()))
        .send()
        .await
        .unwrap();

    assert!(store.object_exists(key).await.unwrap());

    client.delete_object().bucket(BUCKET).key(key).send().await.unwrap();
}

#[tokio::test]
async fn presign_upload_returns_valid_url() {
    let store = setup().await;
    let url = store.presign_upload("integration/upload-test.bin", 300).await.unwrap();
    assert!(url.starts_with("http://localhost:9000"), "got: {url}");
    assert!(url.contains("X-Amz-Signature"));
}

#[tokio::test]
async fn presign_download_returns_valid_url() {
    let store = setup().await;
    let url = store.presign_download("integration/download-test.bin", 300).await.unwrap();
    assert!(url.starts_with("http://localhost:9000"), "got: {url}");
    assert!(url.contains("X-Amz-Signature"));
}

/// PUT via a presigned upload URL actually lands the object.
#[tokio::test]
async fn presign_upload_url_is_usable() {
    let store = setup().await;
    let key = "integration/presign-put-test.bin";
    let body = b"presigned-payload";

    let upload_url = store.presign_upload(key, 300).await.unwrap();

    let http = reqwest::Client::new();
    let resp = http
        .put(&upload_url)
        .body(body.as_ref())
        .send()
        .await
        .expect("HTTP PUT failed");
    assert!(resp.status().is_success(), "PUT status: {}", resp.status());

    assert!(store.object_exists(key).await.unwrap(), "object should exist after presigned PUT");

    raw_client().await.delete_object().bucket(BUCKET).key(key).send().await.unwrap();
}

/// GET via a presigned download URL returns the exact bytes that were uploaded.
#[tokio::test]
async fn presign_download_url_serves_correct_bytes() {
    let store = setup().await;
    let key = "integration/presign-get-test.bin";
    let body = b"download-me";

    raw_client()
        .await
        .put_object()
        .bucket(BUCKET)
        .key(key)
        .body(ByteStream::from(body.to_vec()))
        .send()
        .await
        .unwrap();

    let download_url = store.presign_download(key, 300).await.unwrap();

    let http = reqwest::Client::new();
    let bytes = http
        .get(&download_url)
        .send()
        .await
        .expect("HTTP GET failed")
        .bytes()
        .await
        .unwrap();

    assert_eq!(bytes.as_ref(), body.as_ref());

    raw_client().await.delete_object().bucket(BUCKET).key(key).send().await.unwrap();
}

// ── ArtifactServer with real storage ─────────────────────────────────────────

/// Publish metadata then retrieve it — exercises Registry + proto layer end-to-end.
#[tokio::test]
async fn server_publish_then_get_round_trip() {
    let server = make_server();

    let resp = server
        .publish_artifact(Request::new(PublishRequest {
            model: "humanoid-v2".into(),
            version: "1.0.0".into(),
            sha256: "deadbeef".into(),
            size_bytes: 4096,
            channel: "stable".into(),
            object_key: "humanoid-v2/1.0.0/fw.bin".into(),
            metadata: Default::default(),
        }))
        .await
        .unwrap();

    let artifact_id = resp.into_inner().artifact.unwrap().artifact_id;
    assert!(!artifact_id.is_empty());

    let meta = server
        .get_artifact(Request::new(GetArtifactRequest { artifact_id: artifact_id.clone() }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(meta.artifact_id, artifact_id);
    assert_eq!(meta.version, "1.0.0");
    assert_eq!(meta.sha256, "deadbeef");
}

/// generate_upload_url → PUT via presigned URL → object_exists confirms object is in MinIO.
#[tokio::test]
async fn server_generate_upload_url_then_upload_object() {
    let server = make_server();

    let url_resp = server
        .generate_upload_url(Request::new(UploadUrlRequest {
            model: "arm-v1".into(),
            version: "0.5.0".into(),
            content_type: "application/octet-stream".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    let upload_url = url_resp.upload_url;
    let object_key = url_resp.object_key;
    assert!(!upload_url.is_empty());

    let http = reqwest::Client::new();
    let resp = http
        .put(&upload_url)
        .body(b"firmware-bytes".as_ref())
        .send()
        .await
        .expect("PUT failed");
    assert!(resp.status().is_success(), "PUT status: {}", resp.status());

    let store = MinioStore::new(ENDPOINT, ACCESS, SECRET, BUCKET);
    assert!(store.object_exists(&object_key).await.unwrap(), "object should exist after upload");

    raw_client().await.delete_object().bucket(BUCKET).key(&object_key).send().await.unwrap();
}

/// Publishing the same (model, version) twice must surface as ALREADY_EXISTS.
#[tokio::test]
async fn server_publish_duplicate_returns_already_exists() {
    let server = make_server();

    let req = || {
        Request::new(PublishRequest {
            model: "crawler-v3".into(),
            version: "1.0.0".into(),
            sha256: "abc".into(),
            size_bytes: 100,
            channel: "stable".into(),
            object_key: "crawler-v3/1.0.0/fw.bin".into(),
            metadata: Default::default(),
        })
    };

    server.publish_artifact(req()).await.unwrap();
    let err = server.publish_artifact(req()).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::AlreadyExists);
}

/// list_artifacts returns all published records for a model + channel.
#[tokio::test]
async fn server_list_returns_published_artifacts() {
    let server = make_server();

    for version in ["1.0.0", "2.0.0", "3.0.0"] {
        server
            .publish_artifact(Request::new(PublishRequest {
                model: "sensor-v1".into(),
                version: version.into(),
                sha256: "hash".into(),
                size_bytes: 256,
                channel: "nightly".into(),
                object_key: format!("sensor-v1/{version}/fw.bin"),
                metadata: Default::default(),
            }))
            .await
            .unwrap();
    }

    let results = server
        .list_artifacts(Request::new(ListArtifactsRequest {
            model: "sensor-v1".into(),
            channel: "nightly".into(),
            limit: 10,
        }))
        .await
        .unwrap()
        .into_inner()
        .artifacts;

    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|a| a.model == "sensor-v1"));
}

/// get_latest returns the most recently published version.
#[tokio::test]
async fn server_get_latest_returns_newest_version() {
    let server = make_server();

    for version in ["0.9.0", "1.0.0"] {
        server
            .publish_artifact(Request::new(PublishRequest {
                model: "gripper-v2".into(),
                version: version.into(),
                sha256: "hash".into(),
                size_bytes: 128,
                channel: "stable".into(),
                object_key: format!("gripper-v2/{version}/fw.bin"),
                metadata: Default::default(),
            }))
            .await
            .unwrap();
        // ensure distinct created_at timestamps (SQLite resolution is 1s)
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }

    let latest = server
        .get_latest(Request::new(GetLatestRequest {
            model: "gripper-v2".into(),
            channel: "stable".into(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(latest.version, "1.0.0");
}

use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;

use crate::error::Error;

// ── Trait ─────────────────────────────────────────────────────────────────────

#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn presign_upload(&self, key: &str, expires_secs: u64) -> Result<String, Error>;
    async fn presign_download(&self, key: &str, expires_secs: u64) -> Result<String, Error>;
    async fn object_exists(&self, key: &str) -> Result<bool, Error>;
}

// ── MinIO / S3 implementation ─────────────────────────────────────────────────

pub struct MinioStore {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl MinioStore {
    pub fn new(endpoint: &str, access_key: &str, secret_key: &str, bucket: &str) -> Self {
        let creds = Credentials::new(access_key, secret_key, None, None, "static");
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(endpoint)
            .credentials_provider(creds)
            .region(Region::new("us-east-1"))
            .force_path_style(true)
            .build();
        Self {
            client: aws_sdk_s3::Client::from_conf(config),
            bucket: bucket.to_string(),
        }
    }
}

#[async_trait]
impl ObjectStore for MinioStore {
    async fn presign_upload(&self, key: &str, expires_secs: u64) -> Result<String, Error> {
        let cfg = PresigningConfig::expires_in(Duration::from_secs(expires_secs))
            .map_err(|e| Error::Storage(e.to_string()))?;
        let presigned = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(cfg)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(presigned.uri().to_string())
    }

    async fn presign_download(&self, key: &str, expires_secs: u64) -> Result<String, Error> {
        let cfg = PresigningConfig::expires_in(Duration::from_secs(expires_secs))
            .map_err(|e| Error::Storage(e.to_string()))?;
        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(cfg)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(presigned.uri().to_string())
    }

    async fn object_exists(&self, key: &str) -> Result<bool, Error> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                let is_not_found = e
                    .as_service_error()
                    .map(|se| se.is_not_found())
                    .unwrap_or(false);
                if is_not_found {
                    Ok(false)
                } else {
                    Err(Error::Storage(e.to_string()))
                }
            }
        }
    }
}

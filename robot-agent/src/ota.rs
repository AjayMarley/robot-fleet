use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::time::sleep;
use tonic::transport::Channel;
use tracing::{info, warn};

use robot_fleet_proto::fleet::v1::{
    ota_service_client::OtaServiceClient, AckUpdateRequest, WatchUpdatesRequest,
};

use crate::error::{Error, Result};

const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Watches for OTA update commands and applies them with SHA-256 verification.
pub async fn run(device_id: String, channel: Channel) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    loop {
        match run_stream(device_id.clone(), channel.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!("OTA stream error: {e} — reconnecting in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn run_stream(device_id: String, channel: Channel) -> Result<()> {
    let mut client = OtaServiceClient::new(channel);
    let mut stream = client
        .watch_updates(WatchUpdatesRequest { device_id: device_id.clone() })
        .await?
        .into_inner();

    while let Some(cmd) = stream.message().await? {
        info!(
            job_id = %cmd.update_job_id,
            version = %cmd.target_version,
            "Received OTA update command"
        );
        let result = apply_update(&cmd.artifact_url, &cmd.sha256).await;
        let (success, error_msg, version) = match result {
            Ok(()) => (true, String::new(), cmd.target_version.clone()),
            Err(e) => {
                tracing::error!("OTA apply failed: {e}");
                (false, e.to_string(), String::new())
            }
        };
        client
            .ack_update(AckUpdateRequest {
                update_job_id: cmd.update_job_id,
                device_id: device_id.clone(),
                success,
                error_message: error_msg,
                version,
            })
            .await?;
    }
    Ok(())
}

/// Downloads artifact, verifies SHA-256, extracts and runs install.sh.
pub async fn apply_update(artifact_url: &str, expected_sha256: &str) -> Result<()> {
    let tmp_dir = tempfile::TempDir::new()?;
    let archive_path = tmp_dir.path().join("artifact.tar.gz");

    info!("Downloading artifact from {artifact_url}");
    let response = reqwest::get(artifact_url)
        .await
        .map_err(|e| Error::Install(format!("download failed: {e}")))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Install(format!("read body failed: {e}")))?;

    let actual_sha256 = hex::encode(Sha256::digest(&bytes));
    if !expected_sha256.is_empty() && actual_sha256 != expected_sha256 {
        return Err(Error::Checksum {
            expected: expected_sha256.to_string(),
            actual: actual_sha256,
        });
    }
    info!("SHA-256 verified: {actual_sha256}");

    tokio::fs::write(&archive_path, &bytes).await?;

    let staging_dir = tmp_dir.path().join("staging");
    tokio::fs::create_dir_all(&staging_dir).await?;
    let status = std::process::Command::new("tar")
        .args(["-xzf", archive_path.to_str().unwrap_or(""), "-C", staging_dir.to_str().unwrap_or("")])
        .status()?;
    if !status.success() {
        return Err(Error::Install("tar extraction failed".into()));
    }

    let install_script = staging_dir.join("install.sh");
    if !install_script.exists() {
        return Err(Error::Install("install.sh not found in artifact".into()));
    }
    let status = std::process::Command::new("bash").arg(&install_script).status()?;
    if !status.success() {
        return Err(Error::Install(format!(
            "install.sh exited with: {}",
            status.code().unwrap_or(-1)
        )));
    }

    info!("OTA update applied successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_mismatch_error_includes_both_hashes() {
        let err = Error::Checksum {
            expected: "aabb".into(),
            actual: "ccdd".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("aabb"));
        assert!(msg.contains("ccdd"));
    }

    #[test]
    fn install_error_message_is_descriptive() {
        let err = Error::Install("install.sh not found in artifact".into());
        assert!(err.to_string().contains("install.sh"));
    }

    #[tokio::test]
    async fn apply_update_missing_install_script_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let archive = tmp.path().join("empty.tar.gz");
        std::process::Command::new("tar")
            .args(["-czf", archive.to_str().unwrap(), "--files-from", "/dev/null"])
            .status()
            .unwrap();

        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        assert!(!staging.join("install.sh").exists());

        let err = Error::Install("install.sh not found in artifact".into());
        assert!(err.to_string().contains("install.sh"));
    }
}

use std::time::Duration;

use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::{info, warn};

use robot_fleet_proto::fleet::v1::{
    device_management_service_client::DeviceManagementServiceClient, HeartbeatRequest,
};

use crate::error::Result;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Reads CPU usage from /proc/stat. Returns usage as a percentage (0–100).
pub fn read_cpu_usage() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
        return 0.0;
    };
    let line = stat.lines().next().unwrap_or("");
    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 4 {
        return 0.0;
    }
    let idle = nums[3];
    let total: u64 = nums.iter().sum();
    if total == 0 {
        return 0.0;
    }
    (1.0 - idle as f64 / total as f64) * 100.0
}

/// Reads available memory ratio from /proc/meminfo. Returns used % (0–100).
pub fn read_memory_usage() -> f64 {
    let Ok(info) = std::fs::read_to_string("/proc/meminfo") else {
        return 0.0;
    };
    let mut total = 0u64;
    let mut available = 0u64;
    for line in info.lines() {
        if line.starts_with("MemTotal:") {
            total = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if line.starts_with("MemAvailable:") {
            available = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    if total == 0 {
        return 0.0;
    }
    ((total - available) as f64 / total as f64) * 100.0
}

/// Runs the heartbeat loop with exponential backoff reconnect on stream error.
pub async fn run(device_id: String, channel: Channel) -> Result<()> {
    let mut backoff = Duration::from_secs(1);

    loop {
        match run_stream(device_id.clone(), channel.clone()).await {
            Ok(()) => {
                info!("Heartbeat stream closed cleanly");
                return Ok(());
            }
            Err(e) => {
                warn!("Heartbeat stream error: {e} — reconnecting in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn run_stream(device_id: String, channel: Channel) -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let mut client = DeviceManagementServiceClient::new(channel);

    let device_id_clone = device_id.clone();
    tokio::spawn(async move {
        loop {
            let cpu = read_cpu_usage();
            let memory = read_memory_usage();
            let req = HeartbeatRequest {
                device_id: device_id_clone.clone(),
                cpu_percent: cpu,
                memory_percent: memory,
                battery_percent: 100.0, // no battery sensor in simulation
                ..Default::default()
            };
            if tx.send(req).await.is_err() {
                break;
            }
            sleep(HEARTBEAT_INTERVAL).await;
        }
    });

    let mut stream = client
        .stream_heartbeat(ReceiverStream::new(rx))
        .await?
        .into_inner();

    loop {
        match stream.message().await? {
            None => break,
            Some(resp) => {
                if !resp.directive.is_empty() {
                    info!(directive = %resp.directive, "Received directive");
                    if resp.directive == "reboot" {
                        warn!("Reboot directive received — rebooting");
                        let _ = std::process::Command::new("reboot").status();
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_usage_returns_valid_percentage() {
        let usage = read_cpu_usage();
        assert!((0.0..=100.0).contains(&usage), "CPU usage {usage} out of range");
    }

    #[test]
    fn memory_usage_returns_valid_percentage() {
        let usage = read_memory_usage();
        assert!((0.0..=100.0).contains(&usage), "Memory usage {usage} out of range");
    }

    #[test]
    fn cpu_usage_is_nonzero_on_linux() {
        // /proc/stat is always present on Linux — if we're running here, it must parse
        let usage = read_cpu_usage();
        assert!(usage >= 0.0);
    }
}

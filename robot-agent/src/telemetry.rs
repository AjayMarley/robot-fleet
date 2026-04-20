use std::path::PathBuf;
use std::time::Duration;

use prost_types::Timestamp;
use tokio::io::AsyncReadExt;
use tokio::net::UnixListener;
use tokio::time::sleep;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::{info, warn};

use robot_fleet_proto::fleet::v1::{
    telemetry_service_client::TelemetryServiceClient, JointStatus, TelemetryFrame,
};

use crate::error::Result;

const MAX_BACKOFF: Duration = Duration::from_secs(60);
const SINE_INTERVAL: Duration = Duration::from_millis(100);
const JOINT_NAMES: &[&str] = &[
    "panda_joint1", "panda_joint2", "panda_joint3",
    "panda_joint4", "panda_joint5", "panda_joint6",
];

/// Runs the telemetry loop with exponential backoff reconnect.
/// If $SOCKET_PATH is set and the socket exists, reads from Isaac Sim.
/// Otherwise generates sine-wave joint data.
pub async fn run(device_id: String, channel: Channel, socket_path: Option<PathBuf>) -> Result<()> {
    let mut backoff = Duration::from_secs(1);
    loop {
        let result = match &socket_path {
            Some(path) if path.exists() => {
                run_from_socket(device_id.clone(), channel.clone(), path.clone()).await
            }
            _ => run_sine_wave(device_id.clone(), channel.clone()).await,
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!("Telemetry stream error: {e} — reconnecting in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

/// Reads joint frames from the Unix socket written by the Isaac Sim controller.
///
/// Wire format (matches simulator/controller/robot_controller.py `_encode_frame`):
///   [4B u32 BE] payload_length
///   [8B i64 BE] timestamp_ns
///   [4B u32 BE] joint_count N
///   [N*8B f64 BE] positions_rad
///   [N*8B f64 BE] torques_nm
async fn run_from_socket(device_id: String, channel: Channel, socket_path: PathBuf) -> Result<()> {
    info!("Telemetry: listening on Unix socket {:?}", socket_path);
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    let (mut stream, _) = listener.accept().await?;

    let (tx, rx) = tokio::sync::mpsc::channel::<TelemetryFrame>(32);
    let mut client = TelemetryServiceClient::new(channel);

    let device_id_clone = device_id.clone();
    tokio::spawn(async move {
        loop {
            // Read 4-byte length prefix
            let mut len_buf = [0u8; 4];
            if stream.read_exact(&mut len_buf).await.is_err() {
                break;
            }
            let payload_len = u32::from_be_bytes(len_buf) as usize;
            if payload_len == 0 || payload_len > 1_000_000 {
                break;
            }

            let mut payload = vec![0u8; payload_len];
            if stream.read_exact(&mut payload).await.is_err() {
                break;
            }

            let Some(frame) = parse_frame(&device_id_clone, &payload) else {
                warn!("Failed to parse telemetry frame ({payload_len} bytes)");
                continue;
            };

            if tx.send(frame).await.is_err() {
                break;
            }
        }
    });

    let mut resp_stream = client
        .stream_telemetry(ReceiverStream::new(rx))
        .await?
        .into_inner();

    while let Some(ack) = resp_stream.message().await? {
        if ack.throttle {
            warn!("Telemetry throttle requested by server");
            sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

/// Generates sine-wave joint data when no simulator socket is available.
async fn run_sine_wave(device_id: String, channel: Channel) -> Result<()> {
    info!("Telemetry: sine-wave mode (no simulator socket)");
    let (tx, rx) = tokio::sync::mpsc::channel::<TelemetryFrame>(32);
    let mut client = TelemetryServiceClient::new(channel);

    let device_id_clone = device_id.clone();
    tokio::spawn(async move {
        let n = JOINT_NAMES.len();
        let start = std::time::SystemTime::now();
        loop {
            let elapsed = start.elapsed().unwrap_or_default().as_secs_f64();
            let joints: Vec<JointStatus> = JOINT_NAMES
                .iter()
                .enumerate()
                .map(|(i, name)| JointStatus {
                    name: name.to_string(),
                    position_rad: (elapsed + i as f64 * std::f64::consts::PI / n as f64).sin(),
                    torque_nm: 0.5 * (elapsed + i as f64 * std::f64::consts::PI / n as f64).cos(),
                    temperature_c: 35.0,
                })
                .collect();

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let frame = TelemetryFrame {
                device_id: device_id_clone.clone(),
                recorded_at: Some(Timestamp {
                    seconds: now.as_secs() as i64,
                    nanos: now.subsec_nanos() as i32,
                }),
                joints,
                sensors: vec![],
                metrics: Default::default(),
            };
            if tx.send(frame).await.is_err() {
                break;
            }
            sleep(SINE_INTERVAL).await;
        }
    });

    let mut resp_stream = client
        .stream_telemetry(ReceiverStream::new(rx))
        .await?
        .into_inner();

    while let Some(ack) = resp_stream.message().await? {
        if ack.throttle {
            sleep(Duration::from_millis(500)).await;
        }
    }
    Ok(())
}

/// Parses a telemetry payload (without the 4-byte length prefix).
fn parse_frame(device_id: &str, payload: &[u8]) -> Option<TelemetryFrame> {
    if payload.len() < 12 {
        return None;
    }
    let timestamp_ns = i64::from_be_bytes(payload[0..8].try_into().ok()?);
    let joint_count = u32::from_be_bytes(payload[8..12].try_into().ok()?) as usize;

    let expected_len = 12 + joint_count * 8 * 2;
    if payload.len() < expected_len {
        return None;
    }

    let mut joints = Vec::with_capacity(joint_count);
    for i in 0..joint_count {
        let pos_off = 12 + i * 8;
        let tor_off = 12 + joint_count * 8 + i * 8;
        let position_rad = f64::from_be_bytes(payload[pos_off..pos_off + 8].try_into().ok()?);
        let torque_nm = f64::from_be_bytes(payload[tor_off..tor_off + 8].try_into().ok()?);
        let name = JOINT_NAMES.get(i).copied().unwrap_or("unknown").to_string();
        joints.push(JointStatus {
            name,
            position_rad,
            torque_nm,
            temperature_c: 35.0,
        });
    }

    let seconds = timestamp_ns / 1_000_000_000;
    let nanos = (timestamp_ns % 1_000_000_000) as i32;

    Some(TelemetryFrame {
        device_id: device_id.to_string(),
        recorded_at: Some(Timestamp { seconds, nanos }),
        joints,
        sensors: vec![],
        metrics: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_frame(timestamp_ns: i64, positions: &[f64], torques: &[f64]) -> Vec<u8> {
        let n = positions.len();
        let mut payload = Vec::new();
        payload.extend_from_slice(&timestamp_ns.to_be_bytes());
        payload.extend_from_slice(&(n as u32).to_be_bytes());
        for &p in positions {
            payload.extend_from_slice(&p.to_be_bytes());
        }
        for &t in torques {
            payload.extend_from_slice(&t.to_be_bytes());
        }
        payload
    }

    #[test]
    fn parse_frame_round_trip() {
        let positions = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let torques = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let ts = 1_700_000_000_000_000_000i64;
        let payload = encode_frame(ts, &positions, &torques);

        let frame = parse_frame("dev-001", &payload).unwrap();
        assert_eq!(frame.device_id, "dev-001");
        assert_eq!(frame.joints.len(), 6);
        assert!((frame.joints[0].position_rad - 0.1).abs() < 1e-9);
        assert!((frame.joints[0].torque_nm - 1.0).abs() < 1e-9);
        assert!((frame.joints[5].position_rad - 0.6).abs() < 1e-9);
    }

    #[test]
    fn parse_frame_timestamp_correctly_split() {
        let ts_ns: i64 = 1_700_000_001_500_000_000; // 1.5s past the second
        let payload = encode_frame(ts_ns, &[0.0; 6], &[0.0; 6]);
        let frame = parse_frame("dev", &payload).unwrap();
        let recorded = frame.recorded_at.unwrap();
        assert_eq!(recorded.seconds, 1_700_000_001);
        assert_eq!(recorded.nanos, 500_000_000);
    }

    #[test]
    fn parse_frame_rejects_truncated_payload() {
        assert!(parse_frame("dev", &[0u8; 5]).is_none());
    }

    #[test]
    fn parse_frame_rejects_mismatched_joint_count() {
        // Claims 6 joints but only has data for 3
        let payload = encode_frame(0, &[0.0; 3], &[0.0; 3]);
        // Overwrite joint_count with 6
        let mut bad = payload.clone();
        bad[8..12].copy_from_slice(&6u32.to_be_bytes());
        assert!(parse_frame("dev", &bad).is_none());
    }

    #[test]
    fn joint_names_assigned_from_known_list() {
        let payload = encode_frame(0, &[0.0; 6], &[0.0; 6]);
        let frame = parse_frame("dev", &payload).unwrap();
        assert_eq!(frame.joints[0].name, "panda_joint1");
        assert_eq!(frame.joints[5].name, "panda_joint6");
    }
}

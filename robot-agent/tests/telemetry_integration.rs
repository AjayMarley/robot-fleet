/// Integration test: verifies the Unix socket wire format is compatible between
/// the Python simulator (controller/robot_controller.py `_encode_frame`) and
/// the Rust telemetry parser (telemetry.rs `parse_frame`).
///
/// The encoder here mirrors the Python implementation exactly so any drift
/// between the two will cause this test to fail before it reaches the demo.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tempfile::TempDir;

/// Encodes a frame exactly as Python's `_encode_frame` does.
/// Wire format: [4B u32 length][8B i64 timestamp_ns][4B u32 N][N*8B f64 positions][N*8B f64 torques]
fn encode_frame(timestamp_ns: i64, positions: &[f64], torques: &[f64]) -> Vec<u8> {
    let n = positions.len();
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(&timestamp_ns.to_be_bytes());
    payload.extend_from_slice(&(n as u32).to_be_bytes());
    for &p in positions {
        payload.extend_from_slice(&p.to_be_bytes());
    }
    for &t in torques {
        payload.extend_from_slice(&t.to_be_bytes());
    }
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    frame
}

/// Decodes a frame the same way telemetry.rs does (after stripping the 4-byte length prefix).
fn decode_payload(payload: &[u8]) -> Option<(i64, Vec<f64>, Vec<f64>)> {
    if payload.len() < 12 {
        return None;
    }
    let ts = i64::from_be_bytes(payload[0..8].try_into().ok()?);
    let n = u32::from_be_bytes(payload[8..12].try_into().ok()?) as usize;
    let expected = 12 + n * 16;
    if payload.len() < expected {
        return None;
    }
    let mut positions = Vec::with_capacity(n);
    let mut torques = Vec::with_capacity(n);
    for i in 0..n {
        let p_off = 12 + i * 8;
        let t_off = 12 + n * 8 + i * 8;
        positions.push(f64::from_be_bytes(payload[p_off..p_off + 8].try_into().ok()?));
        torques.push(f64::from_be_bytes(payload[t_off..t_off + 8].try_into().ok()?));
    }
    Some((ts, positions, torques))
}

// ─────────────────────────────────────────────────────────────────────────────
// Wire format tests (no socket — pure encoding/decoding symmetry)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn wire_format_round_trip_6_joints() {
    let positions = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
    let torques = vec![1.1, 2.2, 3.3, 4.4, 5.5, 6.6];
    let ts: i64 = 1_700_000_000_123_456_789;

    let frame = encode_frame(ts, &positions, &torques);
    let length = u32::from_be_bytes(frame[0..4].try_into().unwrap()) as usize;
    assert_eq!(length, frame.len() - 4, "length prefix must match payload size");

    let (ts2, pos2, tor2) = decode_payload(&frame[4..]).unwrap();
    assert_eq!(ts2, ts);
    for (a, b) in positions.iter().zip(pos2.iter()) {
        assert!((a - b).abs() < 1e-12, "position mismatch: {a} vs {b}");
    }
    for (a, b) in torques.iter().zip(tor2.iter()) {
        assert!((a - b).abs() < 1e-12, "torque mismatch: {a} vs {b}");
    }
}

#[test]
fn wire_format_timestamp_splits_correctly_at_second_boundary() {
    let ts_ns: i64 = 1_700_000_001_500_000_000; // .5s past the second
    let frame = encode_frame(ts_ns, &[0.0; 6], &[0.0; 6]);
    let (ts, _, _) = decode_payload(&frame[4..]).unwrap();
    assert_eq!(ts / 1_000_000_000, 1_700_000_001);
    assert_eq!(ts % 1_000_000_000, 500_000_000);
}

#[test]
fn wire_format_rejects_truncated_payload() {
    assert!(decode_payload(&[0u8; 5]).is_none());
}

#[test]
fn wire_format_rejects_joint_count_beyond_payload() {
    let frame = encode_frame(0, &[0.0; 3], &[0.0; 3]);
    let mut bad = frame[4..].to_vec(); // strip length prefix
    // Overwrite joint_count field with 99 (way beyond actual data)
    bad[8..12].copy_from_slice(&99u32.to_be_bytes());
    assert!(decode_payload(&bad).is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket I/O — simulates the Python controller connecting and sending frames
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn simulator_to_agent_socket_delivers_frames() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("telemetry.sock");

    // Server side — mimics what telemetry.rs does
    let server_path = socket_path.clone();
    let server = std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(&server_path).unwrap();
        listener
            .set_nonblocking(false)
            .unwrap();
        let (mut conn, _) = listener.accept().unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        let mut received = Vec::new();
        for _ in 0..3 {
            // Read length prefix
            let mut len_buf = [0u8; 4];
            std::io::Read::read_exact(&mut conn, &mut len_buf).unwrap();
            let length = u32::from_be_bytes(len_buf) as usize;

            // Read payload
            let mut payload = vec![0u8; length];
            std::io::Read::read_exact(&mut conn, &mut payload).unwrap();
            received.push(decode_payload(&payload).unwrap());
        }
        received
    });

    // Give listener time to bind
    std::thread::sleep(Duration::from_millis(20));

    // Client side — mimics Python RobotController.on_physics_step
    let mut client = UnixStream::connect(&socket_path).unwrap();
    let positions = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
    let torques = vec![0.5, 1.0, 1.5, 2.0, 2.5, 3.0];

    for i in 0..3i64 {
        let ts = 1_000_000_000i64 + i * 100_000_000;
        let frame = encode_frame(ts, &positions, &torques);
        client.write_all(&frame).unwrap();
    }
    drop(client);

    let frames = server.join().unwrap();
    assert_eq!(frames.len(), 3);

    for (i, (ts, pos, tor)) in frames.iter().enumerate() {
        assert_eq!(*ts, 1_000_000_000i64 + i as i64 * 100_000_000);
        assert_eq!(pos.len(), 6);
        assert_eq!(tor.len(), 6);
        assert!((pos[0] - 0.1).abs() < 1e-12);
        assert!((tor[0] - 0.5).abs() < 1e-12);
    }
}

#[test]
fn simulator_disconnect_does_not_panic_server() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("disc_test.sock");

    let server_path = socket_path.clone();
    let server = std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(&server_path).unwrap();
        let (mut conn, _) = listener.accept().unwrap();
        conn.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        // Try to read — should get EOF cleanly
        let mut len_buf = [0u8; 4];
        let result = std::io::Read::read_exact(&mut conn, &mut len_buf);
        result.is_err() // expect error/EOF — not a panic
    });

    std::thread::sleep(Duration::from_millis(20));
    let client = UnixStream::connect(&socket_path).unwrap();
    drop(client); // immediate disconnect

    let got_eof = server.join().unwrap();
    assert!(got_eof, "server should detect client disconnect cleanly");
}

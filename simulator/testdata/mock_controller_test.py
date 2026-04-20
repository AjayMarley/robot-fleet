"""Tests for SimulatorBackend and RobotController — no GPU required.

The socket bridge is implemented in robot-agent/src/telemetry.rs (Rust).
These tests verify the Python side: correct wire format, backend composition,
and frame delivery over a Unix socket.
"""

import math
import socket
import struct
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from controller.backends import SineWaveBackend, SimulatorBackend, JOINT_NAMES
from controller.robot_controller import RobotController, _encode_frame, JOINT_COUNT


# ---------------------------------------------------------------------------
# Wire format helpers
# ---------------------------------------------------------------------------

def _decode_frame(payload: bytes):
    """Inverse of _encode_frame — parses the payload (without the 4-byte length prefix)."""
    timestamp_ns, n = struct.unpack_from(">qI", payload, 0)
    offset = struct.calcsize(">qI")
    positions = list(struct.unpack_from(f">{n}d", payload, offset))
    offset += n * 8
    torques = list(struct.unpack_from(f">{n}d", payload, offset))
    return timestamp_ns, positions, torques


# ---------------------------------------------------------------------------
# _encode_frame — matches wire format expected by telemetry.rs
# ---------------------------------------------------------------------------

def test_encode_frame_round_trip():
    positions = [0.1 * i for i in range(JOINT_COUNT)]
    torques = [0.5 * i for i in range(JOINT_COUNT)]
    ts = 1_700_000_000_000_000_000
    wire = _encode_frame(ts, positions, torques)

    length = struct.unpack(">I", wire[:4])[0]
    assert length == len(wire) - 4

    ts2, pos2, tor2 = _decode_frame(wire[4:])
    assert ts2 == ts
    assert all(math.isclose(a, b, rel_tol=1e-9) for a, b in zip(positions, pos2))
    assert all(math.isclose(a, b, rel_tol=1e-9) for a, b in zip(torques, tor2))


def test_encode_frame_length_prefix_matches_payload():
    wire = _encode_frame(0, [0.0] * 3, [1.0] * 3)
    declared = struct.unpack(">I", wire[:4])[0]
    assert declared == len(wire) - 4


def test_encode_frame_carries_torques_not_velocities():
    torques = [9.9] * JOINT_COUNT
    wire = _encode_frame(0, [0.0] * JOINT_COUNT, torques)
    _, _, decoded_torques = _decode_frame(wire[4:])
    assert all(math.isclose(t, 9.9, rel_tol=1e-9) for t in decoded_torques)


# ---------------------------------------------------------------------------
# SineWaveBackend
# ---------------------------------------------------------------------------

def test_sine_wave_backend_implements_interface():
    assert isinstance(SineWaveBackend(), SimulatorBackend)


def test_sine_wave_backend_returns_correct_joint_count():
    b = SineWaveBackend()
    assert len(b.get_joint_positions()) == JOINT_COUNT
    assert len(b.get_joint_torques()) == JOINT_COUNT
    assert len(b.get_joint_names()) == JOINT_COUNT


def test_sine_wave_backend_positions_bounded():
    for v in SineWaveBackend().get_joint_positions():
        assert -1.0 <= v <= 1.0


def test_sine_wave_backend_torques_bounded():
    for v in SineWaveBackend().get_joint_torques():
        assert -1.0 <= v <= 1.0


def test_sine_wave_backend_custom_joint_names():
    names = ["j0", "j1", "j2"]
    b = SineWaveBackend(joint_names=names)
    assert b.get_joint_names() == names
    assert len(b.get_joint_positions()) == 3
    assert len(b.get_joint_torques()) == 3


# ---------------------------------------------------------------------------
# RobotController — composition over inheritance
# ---------------------------------------------------------------------------

def _make_server(tmp_path: Path):
    path = tmp_path / "telemetry.sock"
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(path))
    srv.listen(1)
    srv.settimeout(2.0)
    return srv, path


def test_robot_controller_default_backend_is_sine_wave(tmp_path):
    srv, path = _make_server(tmp_path)
    ctrl = RobotController(socket_path=path, directive_path=tmp_path / "dir.sock")
    ctrl.initialize()
    conn, _ = srv.accept()
    conn.settimeout(2.0)

    ctrl.on_physics_step(0.016)

    length = struct.unpack(">I", conn.recv(4))[0]
    payload = conn.recv(length)
    ts, positions, torques = _decode_frame(payload)

    assert ts > 0
    assert len(positions) == JOINT_COUNT
    assert len(torques) == JOINT_COUNT

    ctrl.shutdown()
    conn.close()
    srv.close()


def test_robot_controller_uses_injected_backend(tmp_path):
    """Values from a custom backend reach the wire unchanged."""
    class FixedBackend(SimulatorBackend):
        def get_joint_names(self): return ["j0", "j1"]
        def get_joint_positions(self): return [1.23, 4.56]
        def get_joint_torques(self): return [7.89, 0.11]

    srv, path = _make_server(tmp_path)
    ctrl = RobotController(backend=FixedBackend(), socket_path=path, directive_path=tmp_path / "dir.sock")
    ctrl.initialize()
    conn, _ = srv.accept()
    conn.settimeout(2.0)

    ctrl.on_physics_step(0.016)

    length = struct.unpack(">I", conn.recv(4))[0]
    payload = conn.recv(length)
    _, positions, torques = _decode_frame(payload)

    assert math.isclose(positions[0], 1.23, rel_tol=1e-9)
    assert math.isclose(torques[0], 7.89, rel_tol=1e-9)

    ctrl.shutdown()
    conn.close()
    srv.close()


def test_robot_controller_consecutive_frames_have_distinct_timestamps(tmp_path):
    srv, path = _make_server(tmp_path)
    ctrl = RobotController(socket_path=path, directive_path=tmp_path / "dir.sock")
    ctrl.initialize()
    conn, _ = srv.accept()
    conn.settimeout(2.0)

    timestamps = []
    for _ in range(3):
        ctrl.on_physics_step(0.016)
        time.sleep(0.01)
        length = struct.unpack(">I", conn.recv(4))[0]
        payload = conn.recv(length)
        ts, _, _ = _decode_frame(payload)
        timestamps.append(ts)

    assert len(set(timestamps)) == 3

    ctrl.shutdown()
    conn.close()
    srv.close()


def test_robot_controller_shutdown_is_idempotent(tmp_path):
    srv, path = _make_server(tmp_path)
    ctrl = RobotController(socket_path=path, directive_path=tmp_path / "dir.sock")
    ctrl.initialize()
    srv.accept()
    ctrl.shutdown()
    ctrl.shutdown()


# ---------------------------------------------------------------------------
# GPU marker
# ---------------------------------------------------------------------------

@pytest.mark.gpu
def test_placeholder_gpu_physics_step():
    pytest.skip("GPU test — run with: pytest -m gpu")

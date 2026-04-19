"""Tests for RobotController and SocketBridge using fake ArticulationController (no GPU required)."""

import asyncio
import math
import socket
import struct
import sys
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).parent.parent))

from controller.robot_controller import (
    ArticulationController,
    RobotController,
    _encode_frame,
    JOINT_COUNT,
)
from controller.socket_bridge import SocketBridge, _read_framed


# ---------------------------------------------------------------------------
# _encode_frame / decode round-trip
# ---------------------------------------------------------------------------

def _decode_frame(payload: bytes):
    timestamp_ns, n = struct.unpack_from(">qI", payload, 0)
    offset = struct.calcsize(">qI")
    positions = list(struct.unpack_from(f">{n}d", payload, offset))
    offset += n * 8
    velocities = list(struct.unpack_from(f">{n}d", payload, offset))
    return timestamp_ns, positions, velocities


def test_encode_frame_round_trip():
    positions = [0.1 * i for i in range(JOINT_COUNT)]
    velocities = [0.2 * i for i in range(JOINT_COUNT)]
    ts = 1_700_000_000_000_000_000
    wire = _encode_frame(ts, positions, velocities)

    # First 4 bytes are the framing length
    length = struct.unpack(">I", wire[:4])[0]
    assert length == len(wire) - 4

    ts2, pos2, vel2 = _decode_frame(wire[4:])
    assert ts2 == ts
    assert all(math.isclose(a, b, rel_tol=1e-9) for a, b in zip(positions, pos2))
    assert all(math.isclose(a, b, rel_tol=1e-9) for a, b in zip(velocities, vel2))


def test_encode_frame_length_prefix_matches_payload():
    wire = _encode_frame(0, [0.0] * 3, [1.0] * 3)
    declared = struct.unpack(">I", wire[:4])[0]
    assert declared == len(wire) - 4


# ---------------------------------------------------------------------------
# ArticulationController stub
# ---------------------------------------------------------------------------

def test_stub_returns_correct_joint_count():
    ctrl = ArticulationController()
    assert len(ctrl.get_joint_positions()) == JOINT_COUNT
    assert len(ctrl.get_joint_velocities()) == JOINT_COUNT


def test_stub_joint_values_are_bounded():
    ctrl = ArticulationController()
    for v in ctrl.get_joint_positions() + ctrl.get_joint_velocities():
        assert -1.0 <= v <= 1.0, f"Joint value {v} out of sine/cosine range"


# ---------------------------------------------------------------------------
# RobotController with a real Unix socket pair
# ---------------------------------------------------------------------------

def _make_socket_pair(tmp_path: Path):
    path = tmp_path / "test_telemetry.sock"
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(path))
    server.listen(1)
    server.settimeout(2.0)
    return server, path


def test_robot_controller_sends_frame_on_physics_step(tmp_path):
    server, path = _make_socket_pair(tmp_path)
    directive_path = tmp_path / "test_directive.sock"

    ctrl = RobotController(socket_path=path, directive_path=directive_path)
    ctrl.initialize()

    conn, _ = server.accept()
    conn.settimeout(2.0)

    ctrl.on_physics_step(0.016)

    raw_len = conn.recv(4)
    assert len(raw_len) == 4
    payload_len = struct.unpack(">I", raw_len)[0]
    payload = conn.recv(payload_len)
    assert len(payload) == payload_len

    ts, positions, velocities = _decode_frame(payload)
    assert ts > 0
    assert len(positions) == JOINT_COUNT
    assert len(velocities) == JOINT_COUNT

    ctrl.shutdown()
    conn.close()
    server.close()


def test_robot_controller_multiple_steps_produce_distinct_frames(tmp_path):
    server, path = _make_socket_pair(tmp_path)
    directive_path = tmp_path / "test_directive2.sock"

    ctrl = RobotController(socket_path=path, directive_path=directive_path)
    ctrl.initialize()
    conn, _ = server.accept()
    conn.settimeout(2.0)

    timestamps = []
    for _ in range(3):
        ctrl.on_physics_step(0.016)
        time.sleep(0.01)
        raw_len = conn.recv(4)
        length = struct.unpack(">I", raw_len)[0]
        payload = conn.recv(length)
        ts, _, _ = _decode_frame(payload)
        timestamps.append(ts)

    assert len(set(timestamps)) == 3, "Consecutive frames must have distinct timestamps"

    ctrl.shutdown()
    conn.close()
    server.close()


def test_robot_controller_shutdown_is_idempotent(tmp_path):
    server, path = _make_socket_pair(tmp_path)
    directive_path = tmp_path / "test_directive3.sock"

    ctrl = RobotController(socket_path=path, directive_path=directive_path)
    ctrl.initialize()
    server.accept()
    ctrl.shutdown()
    ctrl.shutdown()  # must not raise


# ---------------------------------------------------------------------------
# SocketBridge
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_socket_bridge_delivers_telemetry_frame(tmp_path):
    tel_path = tmp_path / "bridge_tel.sock"
    dir_path = tmp_path / "bridge_dir.sock"

    received: list[bytes] = []
    bridge = SocketBridge(
        telemetry_path=tel_path,
        directive_path=dir_path,
        on_frame=received.append,
    )

    server_task = asyncio.create_task(bridge.run())
    await asyncio.sleep(0.05)  # let sockets bind

    # Send one framed telemetry message
    frame = _encode_frame(123456, [0.1] * JOINT_COUNT, [0.2] * JOINT_COUNT)
    reader, writer = await asyncio.open_unix_connection(path=str(tel_path))
    writer.write(frame)
    await writer.drain()
    await asyncio.sleep(0.05)

    assert len(received) == 1
    ts, pos, vel = _decode_frame(received[0])
    assert ts == 123456
    assert len(pos) == JOINT_COUNT

    writer.close()
    server_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await server_task


@pytest.mark.asyncio
async def test_socket_bridge_sends_directive_to_connected_client(tmp_path):
    tel_path = tmp_path / "bridge_tel2.sock"
    dir_path = tmp_path / "bridge_dir2.sock"

    bridge = SocketBridge(telemetry_path=tel_path, directive_path=dir_path)
    server_task = asyncio.create_task(bridge.run())
    await asyncio.sleep(0.05)

    reader, writer = await asyncio.open_unix_connection(path=str(dir_path))
    await asyncio.sleep(0.05)

    await bridge.send_directive("reboot")
    data = await asyncio.wait_for(reader.read(64), timeout=1.0)
    assert data == b"reboot"

    writer.close()
    server_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await server_task


@pytest.mark.asyncio
async def test_socket_bridge_handles_empty_read_gracefully(tmp_path):
    tel_path = tmp_path / "bridge_tel3.sock"
    dir_path = tmp_path / "bridge_dir3.sock"

    bridge = SocketBridge(telemetry_path=tel_path, directive_path=dir_path)
    server_task = asyncio.create_task(bridge.run())
    await asyncio.sleep(0.05)

    reader, writer = await asyncio.open_unix_connection(path=str(tel_path))
    writer.close()  # immediate disconnect — bridge must not crash
    await asyncio.sleep(0.05)

    assert not server_task.done()

    server_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await server_task


# ---------------------------------------------------------------------------
# GPU marker (skipped in CI)
# ---------------------------------------------------------------------------

@pytest.mark.gpu
def test_placeholder_gpu_physics_step():
    """Placeholder — replace with real Isaac Sim ArticulationController when GPU available."""
    pytest.skip("GPU test — run with: pytest -m gpu")

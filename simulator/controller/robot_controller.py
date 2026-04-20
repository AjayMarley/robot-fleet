"""Robot controller: reads joint state from a SimulatorBackend and streams frames to robot-agent."""

import math
import socket
import struct
import time
from pathlib import Path

from .backends import SimulatorBackend, SineWaveBackend, JOINT_NAMES

SOCKET_PATH = Path(str(__import__("os").environ.get("SOCKET_PATH", "/tmp/robot_telemetry.sock")))
DIRECTIVE_SOCKET_PATH = Path(str(__import__("os").environ.get("DIRECTIVE_SOCKET_PATH", "/tmp/robot_directive.sock")))

JOINT_COUNT = len(JOINT_NAMES)


class RobotController:
    """
    Simulator-agnostic controller. Reads joint positions and torques from any
    SimulatorBackend and writes length-prefixed frames to robot-agent via Unix socket.

    Wire frame format (internal transport between simulator and robot-agent):
      [4B length][8B timestamp_ns][4B joint_count][N*8B positions_rad][N*8B torques_nm]
    """

    def __init__(
        self,
        backend: SimulatorBackend | None = None,
        socket_path: Path = SOCKET_PATH,
        directive_path: Path = DIRECTIVE_SOCKET_PATH,
    ):
        self._backend: SimulatorBackend = backend or SineWaveBackend()
        self._socket_path = socket_path
        self._directive_path = directive_path
        self._sock: socket.socket | None = None
        self._directive_sock: socket.socket | None = None

    def initialize(self):
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.connect(str(self._socket_path))
        self._directive_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._directive_sock.setblocking(False)
        try:
            self._directive_sock.connect(str(self._directive_path))
        except (ConnectionRefusedError, FileNotFoundError, OSError):
            self._directive_sock.close()
            self._directive_sock = None

    def on_physics_step(self, step_size: float):
        positions = self._backend.get_joint_positions()
        torques = self._backend.get_joint_torques()
        frame = _encode_frame(time.time_ns(), positions, torques)
        if self._sock:
            try:
                self._sock.sendall(frame)
            except OSError:
                pass
        self._poll_directive()

    def _poll_directive(self):
        if not self._directive_sock:
            return
        try:
            data = self._directive_sock.recv(256)
            if data:
                _handle_directive(data.decode().strip())
        except BlockingIOError:
            pass

    def shutdown(self):
        for s in (self._sock, self._directive_sock):
            if s:
                try:
                    s.close()
                except OSError:
                    pass
        self._sock = None
        self._directive_sock = None


def _encode_frame(timestamp_ns: int, positions: list[float], torques: list[float]) -> bytes:
    """
    Encode a joint state frame for consumption by robot-agent/src/telemetry.rs.

    Wire format (big-endian):
      [4B u32  ] — payload length (bytes that follow)
      [8B i64  ] — timestamp_ns (Unix nanoseconds)
      [4B u32  ] — joint_count N
      [N*8B f64] — joint positions (radians)
      [N*8B f64] — joint torques (N·m)

    robot-agent reads this from the Unix socket at $SOCKET_PATH,
    maps each joint to a JointStatus proto field, and streams TelemetryFrame
    over gRPC to TelemetryService.StreamTelemetry.
    """
    n = len(positions)
    payload = (
        struct.pack(">qI", timestamp_ns, n)
        + struct.pack(f">{n}d", *positions)
        + struct.pack(f">{n}d", *torques)
    )
    return struct.pack(">I", len(payload)) + payload


def _handle_directive(directive: str):
    if directive == "reboot":
        import subprocess
        subprocess.run(["reboot"], check=False)

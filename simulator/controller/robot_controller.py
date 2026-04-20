"""Robot controller: reads joint state from a SimulatorBackend and streams frames to robot-agent."""

import math
import os
import socket
import struct
import time
from pathlib import Path

try:
    from .backends import SimulatorBackend, SineWaveBackend, JOINT_NAMES
except ImportError:
    from backends import SimulatorBackend, SineWaveBackend, JOINT_NAMES

# Unix socket paths (Linux / container)
SOCKET_PATH = Path(os.environ.get("SOCKET_PATH", "/tmp/robot_telemetry.sock"))
DIRECTIVE_SOCKET_PATH = Path(os.environ.get("DIRECTIVE_SOCKET_PATH", "/tmp/robot_directive.sock"))

# TCP endpoint for Windows Isaac Sim → WSL2 robot-agent
# Set SOCKET_MODE=tcp and ROBOT_AGENT_HOST/ROBOT_AGENT_PORT to enable.
SOCKET_MODE = os.environ.get("SOCKET_MODE", "unix")   # "unix" | "tcp"
ROBOT_AGENT_HOST = os.environ.get("ROBOT_AGENT_HOST", "127.0.0.1")
ROBOT_AGENT_PORT = int(os.environ.get("ROBOT_AGENT_PORT", "7777"))

JOINT_COUNT = len(JOINT_NAMES)


class RobotController:
    """
    Simulator-agnostic controller. Reads joint positions and torques from any
    SimulatorBackend and writes length-prefixed frames to robot-agent.

    Transport is selected by environment:
      SOCKET_MODE=unix  (default) — AF_UNIX socket at $SOCKET_PATH (Linux only)
      SOCKET_MODE=tcp             — TCP to $ROBOT_AGENT_HOST:$ROBOT_AGENT_PORT
                                    (use this when Isaac Sim is on Windows and
                                     robot-agent is in WSL2 listening on TCP_LISTEN_ADDR)

    Wire frame format (shared with robot-agent/src/telemetry.rs):
      [4B u32 BE] payload_length
      [8B i64 BE] timestamp_ns
      [4B u32 BE] joint_count N
      [N*8B f64 BE] positions_rad
      [N*8B f64 BE] torques_nm
    """

    def __init__(
        self,
        backend: SimulatorBackend | None = None,
        socket_path: Path = SOCKET_PATH,
        directive_path: Path = DIRECTIVE_SOCKET_PATH,
        socket_mode: str = SOCKET_MODE,
        tcp_host: str = ROBOT_AGENT_HOST,
        tcp_port: int = ROBOT_AGENT_PORT,
    ):
        self._backend: SimulatorBackend = backend or SineWaveBackend()
        self._socket_path = socket_path
        self._directive_path = directive_path
        self._mode = socket_mode
        self._tcp_host = tcp_host
        self._tcp_port = tcp_port
        self._sock: socket.socket | None = None
        self._directive_sock: socket.socket | None = None

    def initialize(self):
        if self._mode == "tcp":
            self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self._sock.connect((self._tcp_host, self._tcp_port))
        else:
            self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._sock.connect(str(self._socket_path))
            # Directive socket only available in Unix mode
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
    Encode a joint state frame for robot-agent/src/telemetry.rs.

    Wire format (big-endian):
      [4B u32  ] payload length (bytes that follow)
      [8B i64  ] timestamp_ns (Unix nanoseconds)
      [4B u32  ] joint_count N
      [N*8B f64] joint positions (radians)
      [N*8B f64] joint torques (N·m)
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

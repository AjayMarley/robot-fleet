"""Robot articulation controller that captures joint state and bridges to robot-agent via Unix socket."""

import asyncio
import math
import socket
import struct
import time
from pathlib import Path

SOCKET_PATH = Path("/tmp/robot_telemetry.sock")
DIRECTIVE_SOCKET_PATH = Path("/tmp/robot_directive.sock")
JOINT_COUNT = 6


class ArticulationController:
    """Stub base class — replaced by isaac_sim.ArticulationController in real Isaac Sim."""

    def initialize(self):
        pass

    def get_joint_positions(self) -> list[float]:
        t = time.time()
        return [math.sin(t + i * math.pi / JOINT_COUNT) for i in range(JOINT_COUNT)]

    def get_joint_velocities(self) -> list[float]:
        t = time.time()
        return [math.cos(t + i * math.pi / JOINT_COUNT) for i in range(JOINT_COUNT)]


try:
    # Available when running under /isaac-sim/kit/python/python3 inside the container
    from omni.isaac.core.controllers import ArticulationController as IsaacArticulationController  # type: ignore
    _Base = IsaacArticulationController
except ImportError:
    _Base = ArticulationController


class RobotController(_Base):
    """Physics-step controller that streams joint state to robot-agent via Unix socket."""

    def __init__(self, socket_path: Path = SOCKET_PATH, directive_path: Path = DIRECTIVE_SOCKET_PATH):
        super().__init__()
        self._socket_path = socket_path
        self._directive_path = directive_path
        self._sock: socket.socket | None = None
        self._directive_sock: socket.socket | None = None
        self._pending_directive: str = ""

    def initialize(self):
        super().initialize()
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
        positions = self.get_joint_positions()
        velocities = self.get_joint_velocities()
        timestamp_ns = time.time_ns()
        frame = _encode_frame(timestamp_ns, positions, velocities)
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
                self._pending_directive = data.decode().strip()
                _handle_directive(self._pending_directive)
        except BlockingIOError:
            pass

    def shutdown(self):
        for s in (self._sock, self._directive_sock):
            if s:
                try:
                    s.close()
                except OSError:
                    pass


def _encode_frame(timestamp_ns: int, positions: list[float], velocities: list[float]) -> bytes:
    """Wire format: [8B timestamp_ns][4B joint_count][N*8B positions][N*8B velocities]"""
    n = len(positions)
    header = struct.pack(">qI", timestamp_ns, n)
    pos_bytes = struct.pack(f">{n}d", *positions)
    vel_bytes = struct.pack(f">{n}d", *velocities)
    payload = header + pos_bytes + vel_bytes
    # Prefix with 4-byte length for framing
    return struct.pack(">I", len(payload)) + payload


def _handle_directive(directive: str):
    if directive == "reboot":
        import subprocess
        subprocess.run(["reboot"], check=False)

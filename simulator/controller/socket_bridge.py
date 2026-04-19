"""Async Unix socket bridge: relays telemetry frames from Isaac Sim controller to robot-agent gRPC."""

import asyncio
import struct
from pathlib import Path

TELEMETRY_SOCKET = Path("/tmp/robot_telemetry.sock")
DIRECTIVE_SOCKET = Path("/tmp/robot_directive.sock")


class SocketBridge:
    """
    Listens on two Unix sockets:
      - TELEMETRY_SOCKET  — receives encoded frames from RobotController, forwards to robot-agent
      - DIRECTIVE_SOCKET  — receives directives from robot-agent, forwards to RobotController
    """

    def __init__(
        self,
        telemetry_path: Path = TELEMETRY_SOCKET,
        directive_path: Path = DIRECTIVE_SOCKET,
        on_frame=None,
        on_directive=None,
    ):
        self._telemetry_path = telemetry_path
        self._directive_path = directive_path
        self._on_frame = on_frame or (lambda f: None)
        self._on_directive = on_directive or (lambda d: None)
        self._directive_writers: list[asyncio.StreamWriter] = []

    async def run(self):
        for path in (self._telemetry_path, self._directive_path):
            if path.exists():
                path.unlink()

        await asyncio.gather(
            asyncio.start_unix_server(self._handle_telemetry, path=str(self._telemetry_path)),
            asyncio.start_unix_server(self._handle_directive_client, path=str(self._directive_path)),
        )
        await asyncio.Event().wait()

    async def _handle_telemetry(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        try:
            while True:
                frame = await _read_framed(reader)
                if frame is None:
                    break
                self._on_frame(frame)
        finally:
            writer.close()

    async def _handle_directive_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        self._directive_writers.append(writer)
        try:
            await reader.read(1)  # block until client disconnects
        finally:
            self._directive_writers.remove(writer)
            writer.close()

    async def send_directive(self, directive: str):
        data = directive.encode()
        for writer in list(self._directive_writers):
            try:
                writer.write(data)
                await writer.drain()
            except OSError:
                pass


async def _read_framed(reader: asyncio.StreamReader) -> bytes | None:
    """Read a length-prefixed frame (4-byte big-endian length header)."""
    try:
        header = await reader.readexactly(4)
    except asyncio.IncompleteReadError:
        return None
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > 1_000_000:
        return None
    try:
        return await reader.readexactly(length)
    except asyncio.IncompleteReadError:
        return None


if __name__ == "__main__":
    bridge = SocketBridge()
    asyncio.run(bridge.run())

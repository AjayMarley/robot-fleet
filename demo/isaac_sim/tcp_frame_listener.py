"""
Minimal TCP frame listener — run in WSL2 to verify Isaac Sim telemetry pipe.

Usage:
    python3 demo/isaac_sim/tcp_frame_listener.py

Then press Play in Isaac Sim. Should print 10 frames and exit.
"""

import socket
import struct

HOST = "0.0.0.0"
PORT = 7777

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind((HOST, PORT))
srv.listen(1)
print(f"Waiting for Isaac Sim on {HOST}:{PORT} ...")

conn, addr = srv.accept()
print(f"Connected from {addr}")

frame_count = 0
while frame_count < 10:
    hdr = conn.recv(4)
    if not hdr:
        break
    n_bytes = struct.unpack(">I", hdr)[0]
    payload = conn.recv(n_bytes)
    ts   = struct.unpack_from(">q", payload, 0)[0]
    n    = struct.unpack_from(">I", payload, 8)[0]
    pos0 = struct.unpack_from(">d", payload, 12)[0]
    print(f"frame {frame_count + 1:>2}: {n} joints  joint[0]={pos0:.4f} rad  ts={ts}")
    frame_count += 1

conn.close()
print("Done — TCP pipe verified")

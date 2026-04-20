"""
Run on Windows to verify tcp_frame_listener.py can receive frames.
No Isaac Sim required.

Usage (PowerShell):
    python tcp_test_sender.py --host 192.168.50.155 --port 7777
"""

import argparse
import socket
import struct
import time
import math

parser = argparse.ArgumentParser()
parser.add_argument("--host", default="192.168.50.155")
parser.add_argument("--port", type=int, default=7777)
args = parser.parse_args()

JOINTS = 19

def encode_frame(timestamp_ns, positions, torques):
    n = len(positions)
    payload = struct.pack(">qI", timestamp_ns, n)
    payload += struct.pack(f">{n}d", *positions)
    payload += struct.pack(f">{n}d", *torques)
    return struct.pack(">I", len(payload)) + payload

print(f"Connecting to {args.host}:{args.port} ...")
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect((args.host, args.port))
print("Connected — sending 10 frames")

for i in range(10):
    t = time.time()
    positions = [math.sin(t + j * 0.3) for j in range(JOINTS)]
    torques   = [math.cos(t + j * 0.3) * 0.5 for j in range(JOINTS)]
    frame = encode_frame(time.time_ns(), positions, torques)
    sock.sendall(frame)
    print(f"sent frame {i+1}")
    time.sleep(0.1)

sock.close()
print("Done")

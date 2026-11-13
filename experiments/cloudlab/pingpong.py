#!/usr/bin/env python3
"""Minimal TCP ping-pong with TCP_NODELAY: 64-byte request, 64-byte echo.
A userspace Python loop: its round-trip numbers include interpreter and
syscall overhead and are labelled as such; they are a kernel-TCP floor for
this script, not a NIC latency claim. Server: `pingpong.py server HOST:PORT`;
client: `pingpong.py client HOST:PORT --seconds 30` prints JSON."""

import json
import socket
import sys
import time

SIZE = 64


def address(text):
    host, port = text.rsplit(":", 1)
    return host, int(port)


def serve(host, port):
    with socket.create_server((host, port), reuse_port=False) as server:
        server.settimeout(600)
        conn, _ = server.accept()
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        with conn:
            while True:
                data = conn.recv(SIZE, socket.MSG_WAITALL) if hasattr(socket, "MSG_WAITALL") else conn.recv(SIZE)
                if not data:
                    return
                conn.sendall(data)


def percentiles(samples):
    samples = sorted(samples)
    out = {}
    for p in (50, 90, 99, 99.9, 99.99):
        index = min(len(samples) - 1, max(0, round(p / 100 * len(samples)) - 1))
        out[str(p)] = samples[index]
    return out


def client(host, port, seconds, warmup=1000):
    deadline = time.monotonic() + 300
    while True:
        try:
            sock = socket.create_connection((host, port), timeout=10)
            break
        except OSError:
            if time.monotonic() > deadline:
                raise
            time.sleep(1)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    payload = b"x" * SIZE
    samples = []
    with sock:
        for _ in range(warmup):
            sock.sendall(payload)
            sock.recv(SIZE, socket.MSG_WAITALL)
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            start = time.perf_counter_ns()
            sock.sendall(payload)
            sock.recv(SIZE, socket.MSG_WAITALL)
            samples.append(time.perf_counter_ns() - start)
    return {
        "tool": "python-tcp-pingpong",
        "note": "userspace Python loop with TCP_NODELAY; includes interpreter overhead; not a NIC or kernel floor",
        "message_bytes": SIZE,
        "warmup": warmup,
        "seconds": seconds,
        "round_trips": len(samples),
        "rtt_ns": {"min": min(samples), "mean": sum(samples) / len(samples), "max": max(samples), "percentile": percentiles(samples)},
        "peer": f"{host}:{port}",
    }


def main():
    role, target = sys.argv[1], address(sys.argv[2])
    seconds = int(sys.argv[sys.argv.index("--seconds") + 1]) if "--seconds" in sys.argv else 30
    if role == "server":
        serve(*target)
    elif role == "client":
        print(json.dumps(client(*target, seconds), indent=1))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()

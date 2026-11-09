"""One owned TCP stream per ZFS send/receive, with receiver completion ACK."""

import argparse
import hashlib
import json
import socket
import subprocess
import time
from pathlib import Path


def receive(listener, token, argv):
    deadline = time.monotonic() + 600
    while True:
        listener.settimeout(max(0.01, deadline - time.monotonic()))
        stream, _ = listener.accept()
        stream.settimeout(600)
        supplied = b""
        while len(supplied) < len(token):
            part = stream.recv(len(token) - len(supplied))
            if not part:
                break
            supplied += part
        if supplied == token:
            break
        stream.close()
    with stream, subprocess.Popen(argv, stdin=subprocess.PIPE) as receiver:
        digest = hashlib.sha256()
        count = 0
        try:
            while part := stream.recv(1024 * 1024):
                receiver.stdin.write(part)
                digest.update(part)
                count += len(part)
            receiver.stdin.close()
            status = receiver.wait(timeout=600)
            result = {"bytes": count, "sha256": digest.hexdigest(), "status": status}
            reply = json.dumps(result).encode() + b"\n"
            stream.sendall(reply)
            if status:
                raise RuntimeError(f"ZFS receive failed: {status}")
            return {**result, "application_bytes": len(token) + count + len(reply)}
        finally:
            if receiver.poll() is None:
                receiver.kill()
                receiver.wait()


def send(address, token, argv):
    start = time.monotonic_ns()
    with socket.create_connection(address, timeout=600) as stream:
        stream.sendall(token)
        digest = hashlib.sha256()
        count = 0
        with subprocess.Popen(argv, stdout=subprocess.PIPE) as sender:
            try:
                while part := sender.stdout.read(1024 * 1024):
                    stream.sendall(part)
                    digest.update(part)
                    count += len(part)
                status = sender.wait(timeout=600)
                stream.shutdown(socket.SHUT_WR)
                reply = b""
                while not reply.endswith(b"\n"):
                    part = stream.recv(4096 - len(reply))
                    if not part or len(reply) + len(part) >= 4096:
                        raise RuntimeError("missing or oversized receive ACK")
                    reply += part
                received = json.loads(reply)
                if status or received != {
                    "bytes": count,
                    "sha256": digest.hexdigest(),
                    "status": 0,
                }:
                    raise RuntimeError(
                        f"ZFS stream failed: send={status}, receive={received}"
                    )
                return {
                    "elapsed_ns": time.monotonic_ns() - start,
                    "stream_bytes": count,
                    "application_bytes": len(token) + count + len(reply),
                    "sha256": digest.hexdigest(),
                    "argv": argv,
                    "receiver": received,
                }
            finally:
                if sender.poll() is None:
                    sender.kill()
                    sender.wait()


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("--mode", choices=["send", "receive"], required=True)
    parser.add_argument("--fd", type=int)
    parser.add_argument("--host")
    parser.add_argument("--port", type=int)
    parser.add_argument("--token", required=True)
    parser.add_argument("--result", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    argv = args.command[1:] if args.command[:1] == ["--"] else args.command
    if args.mode == "receive":
        with socket.socket(fileno=args.fd) as listener:
            result = receive(listener, args.token.encode(), argv)
    else:
        result = send((args.host, args.port), args.token.encode(), argv)
    Path(args.result).write_text(json.dumps(result) + "\n")


if __name__ == "__main__":
    main()

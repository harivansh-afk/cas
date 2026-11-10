"""Transport ACK tests; actual ZFS semantics are verified by the guest fixture."""

import json
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path

from zfs_stream import receive, send


class StreamTests(unittest.TestCase):
    def test_stream_counts_real_tcp_bytes_and_requires_successful_receive(self):
        for status in (0, 7):
            with (
                tempfile.TemporaryDirectory() as directory,
                socket.socket() as listener,
            ):
                listener.bind(("127.0.0.1", 0))
                listener.listen(1)
                path = Path(directory) / "received"
                received = []
                errors = []

                def worker(
                    listener=listener,
                    received=received,
                    errors=errors,
                    path=path,
                    status=status,
                ):
                    try:
                        received.append(
                            receive(
                                listener,
                                b"secret",
                                [
                                    sys.executable,
                                    "-c",
                                    "import pathlib,sys; pathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read()); sys.exit(int(sys.argv[2]))",
                                    str(path),
                                    str(status),
                                ],
                            )
                        )
                    except RuntimeError as error:
                        errors.append(error)

                thread = threading.Thread(target=worker)
                thread.start()
                command = [
                    sys.executable,
                    "-c",
                    "import sys; sys.stdout.buffer.write(bytes(range(256))*8192)",
                ]
                try:
                    if status:
                        with self.assertRaises(RuntimeError):
                            send(listener.getsockname(), b"secret", command)
                    else:
                        sent = send(listener.getsockname(), b"secret", command)
                        self.assertEqual(sent["stream_bytes"], 256 * 8192)
                        self.assertGreater(
                            sent["application_bytes"], sent["stream_bytes"]
                        )
                finally:
                    thread.join(timeout=5)
                self.assertFalse(thread.is_alive())
                self.assertEqual(path.read_bytes(), bytes(range(256)) * 8192)
                if status:
                    self.assertEqual(len(errors), 1)
                else:
                    self.assertEqual(
                        received[0]["application_bytes"], sent["application_bytes"]
                    )

    def test_cli_forwards_stream_commands_and_inherited_listener(self):
        with tempfile.TemporaryDirectory() as directory, socket.socket() as listener:
            root = Path(directory)
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            script = str(Path(__file__).with_name("zfs_stream.py"))
            receiver = subprocess.Popen(
                [
                    sys.executable,
                    script,
                    "--mode",
                    "receive",
                    "--fd",
                    str(listener.fileno()),
                    "--token",
                    "secret",
                    "--result",
                    str(root / "received.json"),
                    "--",
                    sys.executable,
                    "-c",
                    "import pathlib,sys; pathlib.Path(sys.argv[1]).write_bytes(sys.stdin.buffer.read())",
                    str(root / "bytes"),
                ],
                pass_fds=(listener.fileno(),),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            try:
                sender = subprocess.run(
                    [
                        sys.executable,
                        script,
                        "--mode",
                        "send",
                        "--host",
                        "127.0.0.1",
                        "--port",
                        str(listener.getsockname()[1]),
                        "--token",
                        "secret",
                        "--result",
                        str(root / "sent.json"),
                        "--",
                        sys.executable,
                        "-c",
                        "import sys; sys.stdout.buffer.write(b'x'*4096)",
                    ],
                    capture_output=True,
                    timeout=10,
                    check=False,
                )
                self.assertEqual(sender.returncode, 0, sender.stderr)
                _, error = receiver.communicate(timeout=10)
                self.assertEqual(receiver.returncode, 0, error)
                self.assertEqual((root / "bytes").read_bytes(), b"x" * 4096)
                self.assertEqual(
                    json.loads((root / "sent.json").read_text())["stream_bytes"], 4096
                )
            finally:
                if receiver.poll() is None:
                    receiver.kill()
                receiver.communicate(timeout=10)

    def test_eof_without_receiver_ack_is_failure(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)

            def worker():
                with listener.accept()[0] as stream:
                    while stream.recv(4096):
                        pass

            thread = threading.Thread(target=worker)
            thread.start()
            try:
                with self.assertRaisesRegex(RuntimeError, "ACK"):
                    send(
                        listener.getsockname(),
                        b"secret",
                        [sys.executable, "-c", "print('data')"],
                    )
            finally:
                thread.join(timeout=5)
            self.assertFalse(thread.is_alive())


if __name__ == "__main__":
    unittest.main()

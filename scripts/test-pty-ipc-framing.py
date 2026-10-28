#!/usr/bin/env python3
"""Regression tests for partial IPC socket reads in the PTY acceptance client."""
import importlib.util
from pathlib import Path
import socket
import struct
import threading
import unittest

SOURCE = Path(__file__).with_name("test-pty-daemon-recovery.py")
spec = importlib.util.spec_from_file_location("pty_daemon_recovery", SOURCE)
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)


class FrameReadTests(unittest.TestCase):
    def test_fragmented_header_and_payload(self):
        sender, receiver = socket.socketpair()
        payload = b'{"type":"ok","data":{"output_base64":"' + b"A" * 65536 + b'"}}'
        frame = struct.pack("<I", len(payload)) + payload

        def transmit():
            try:
                for part in [frame[:1], frame[1:3], frame[3:4], frame[4:100], frame[100:]]:
                    sender.sendall(part)
            finally:
                sender.close()

        thread = threading.Thread(target=transmit)
        thread.start()
        try:
            header = recovery.recv_exact(receiver, 4)
            body = recovery.recv_exact(receiver, struct.unpack("<I", header)[0])
            self.assertEqual(body, payload)
        finally:
            receiver.close()
            thread.join()

    def test_truncated_body_is_rejected_with_counts(self):
        sender, receiver = socket.socketpair()
        sender.sendall(b"abc")
        sender.close()
        try:
            with self.assertRaisesRegex(ConnectionError, "received 3/10 bytes"):
                recovery.recv_exact(receiver, 10)
        finally:
            receiver.close()

    def test_truncated_header_is_rejected(self):
        sender, receiver = socket.socketpair()
        sender.sendall(b"\x01\x02")
        sender.close()
        try:
            with self.assertRaisesRegex(ConnectionError, "received 2/4 bytes"):
                recovery.recv_exact(receiver, 4)
        finally:
            receiver.close()


if __name__ == "__main__":
    unittest.main()

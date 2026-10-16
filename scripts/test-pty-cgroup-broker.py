#!/usr/bin/env python3
"""Unprivileged protocol negative tests; real kernel acceptance stays required."""
import importlib.util
import socket
import os
import tempfile
import threading
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "pty_cgroup_broker", Path(__file__).with_name("pty-cgroup-broker.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BrokerProtocolTests(unittest.TestCase):
    def setUp(self):
        self.broker = module.CgroupBroker.__new__(module.CgroupBroker)
        self.broker.authorized_uid = 1000
        self.broker.owned = set()

    def test_fragmented_request_is_reassembled(self):
        left, right = socket.socketpair()
        try:
            def writer():
                left.sendall(b'{"op":"reserve",')
                left.sendall(b'"name":"pty-session-' + b'a' * 32 + b'"}\n')
                left.close()
            thread = threading.Thread(target=writer)
            thread.start()
            response = module.read_request(right)
            thread.join(timeout=2)
            self.assertEqual(response["op"], "reserve")
            self.assertEqual(response["name"], "pty-session-" + "a" * 32)
        finally:
            left.close()
            right.close()

    def test_truncated_or_multiple_commands_are_rejected(self):
        for raw in (b'{"op":"reserve"', b'{}\n{}\n',
                    b'{}\ntrailing', b'\n', b'x' * 4097 + b'\n'):
            left, right = socket.socketpair()
            try:
                left.sendall(raw)
                left.shutdown(socket.SHUT_WR)
                with self.assertRaises(ValueError):
                    module.read_request(right)
            finally:
                left.close()
                right.close()

    def test_session_quota_fails_without_mutating_cgroups(self):
        self.broker.owned = {
            "pty-session-" + format(i, "032x") for i in range(module.MAX_SESSIONS)
        }
        with self.assertRaisesRegex(RuntimeError, "quota"):
            self.broker.execute(
                {"op": "reserve", "name": "pty-session-" + "f" * 32}, 1000
            )

    def test_reap_rejects_replaced_directory_before_kill(self):
        name = "pty-session-" + "a" * 32
        with tempfile.TemporaryDirectory() as folder:
            with tempfile.TemporaryDirectory() as archive:
                root_fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY)
                try:
                    os.mkdir(name, dir_fd=root_fd)
                    original = os.stat(name, dir_fd=root_fd)
                    self.broker.root_fd = root_fd
                    self.broker.owned = {name}
                    self.broker.identities = {name: (original.st_dev, original.st_ino)}
                    os.rename(os.path.join(folder, name), os.path.join(archive, name))
                    os.mkdir(name, dir_fd=root_fd)
                    with self.assertRaisesRegex(RuntimeError, "identity changed"):
                        self.broker.execute({"op": "reap", "name": name}, 1000)
                    self.assertIn(name, self.broker.owned)
                    self.assertTrue(os.path.isdir(os.path.join(folder, name)))
                finally:
                    os.close(root_fd)

    def test_reap_requires_pinned_reservation_identity(self):
        name = "pty-session-" + "b" * 32
        self.broker.owned = {name}
        self.broker.identities = {}
        with self.assertRaises(PermissionError):
            self.broker.execute({"op": "reap", "name": name}, 1000)

    def test_other_uid_cannot_operate(self):
        with self.assertRaises(PermissionError):
            self.broker.execute(
                {"op": "reserve", "name": "pty-session-" + "a" * 32}, 1001
            )

    def test_broad_or_traversing_names_rejected(self):
        for value in ("../management", "/sys/fs/cgroup", "management",
                      "pty-session-" + "a" * 31, "pty-session-" + "G" * 32):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.broker.execute({"op": "reserve", "name": value}, 1000)

    def test_no_pid_move_or_arbitrary_signal_interface(self):
        name = "pty-session-" + "a" * 32
        for request in (
            {"op": "move", "name": name, "pid": 123},
            {"op": "signal", "name": name},
            {"op": "reserve", "name": name, "pid": 123},
            {"op": "reap", "name": name, "path": "/sys/fs/cgroup"},
        ):
            with self.subTest(request=request), self.assertRaises(ValueError):
                self.broker.execute(request, 1000)

    def test_unowned_reap_denied(self):
        with self.assertRaises(PermissionError):
            self.broker.execute({"op": "reap", "name": "pty-session-" + "b" * 32}, 1000)

    def test_wrong_json_shapes_rejected(self):
        for value in ([], None, "reserve", 12, {"op": "reserve"}, {"op": 2, "name": "x"}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.broker.execute(value, 1000)


if __name__ == "__main__":
    unittest.main()

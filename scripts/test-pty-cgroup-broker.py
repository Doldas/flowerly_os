#!/usr/bin/env python3
"""Unprivileged protocol negative tests; real kernel acceptance stays required."""
import importlib.util
import socket
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

    def test_framing_accepts_fragmented_single_request(self):
        reader, writer = socket.socketpair()
        try:
            writer.sendall(b'{"op":"reserve",')
            writer.sendall(b'"name":"pty-session-' + b'a' * 32 + b'"}\n')
            request = module.read_request(reader)
            self.assertEqual(request["op"], "reserve")
        finally:
            reader.close()
            writer.close()

    def test_framing_rejects_trailing_command(self):
        reader, writer = socket.socketpair()
        try:
            writer.sendall(b'{"op":"reserve"}\n{"op":"reap"}\n')
            with self.assertRaises(ValueError):
                module.read_request(reader)
        finally:
            reader.close()
            writer.close()

    def test_framing_rejects_oversized_or_truncated_request(self):
        for payload in (b'x' * (module.MAX_REQUEST + 1), b'{"op":'):
            reader, writer = socket.socketpair()
            try:
                writer.sendall(payload)
                writer.shutdown(socket.SHUT_WR)
                with self.assertRaises(ValueError):
                    module.read_request(reader)
            finally:
                reader.close()
                writer.close()

    def test_session_quota_fails_without_mutating_cgroups(self):
        self.broker.owned = {"pty-session-" + format(i, "032x") for i in range(module.MAX_SESSIONS)}
        with self.assertRaisesRegex(RuntimeError, "quota"):
            self.broker.execute(
                {"op": "reserve", "name": "pty-session-" + "f" * 32}, 1000
            )

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

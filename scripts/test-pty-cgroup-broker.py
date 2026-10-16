#!/usr/bin/env python3
"""Unprivileged protocol negative tests; real kernel acceptance stays required."""
import importlib.util
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

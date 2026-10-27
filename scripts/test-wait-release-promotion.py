#!/usr/bin/env python3
"""Offline regression tests for the bounded exact-SHA promotion wait."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "flowerly_wait", Path(__file__).with_name("wait-release-promotion.py"))
wait = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wait)
SHA = "a" * 40


def client_for(states):
    def client(method, path):
        if path.startswith("/actions/workflows/"):
            workflow = path.split("/")[3]
            status, conclusion = states[workflow]
            return {"workflow_runs": [{"head_sha": SHA, "head_branch": "release/2",
                                      "event": "push", "status": status,
                                      "conclusion": conclusion}]}
        raise AssertionError(path)
    return client


class PromotionWaitTests(unittest.TestCase):
    def test_retries_until_exact_sha_is_verified(self):
        pending = ValueError("ci.yml: no successful completed PUSH run on release/2@" + SHA)
        results = [pending, True]
        with patch.object(wait.forward, "verify_master", side_effect=results) as verify:
            clocks = iter([0, 1])
            self.assertTrue(wait.await_promotion(
                "release/2", SHA, timeout=60, interval=0,
                client=client_for({k: ("in_progress", None) for k in wait.WORKFLOWS}),
                clock=lambda: next(clocks), pause=lambda _: None))
            self.assertEqual(verify.call_count, 2)

    def test_fails_closed_when_push_fails(self):
        with patch.object(wait.forward, "verify_master",
                          side_effect=ValueError("ci.yml: no successful completed PUSH run")):
            states = {k: ("in_progress", None) for k in wait.WORKFLOWS}
            states["ci.yml"] = ("completed", "failure")
            with self.assertRaisesRegex(ValueError, "completed unsuccessfully"):
                wait.await_promotion("release/2", SHA, timeout=30,
                                     client=client_for(states), clock=lambda: 0)

    def test_timeout_is_explicit(self):
        with patch.object(wait.forward, "verify_master",
                          side_effect=ValueError("ci.yml: no successful completed PUSH run")):
            with self.assertRaisesRegex(ValueError, "timed out"):
                wait.await_promotion(
                    "release/2", SHA, timeout=0,
                    client=client_for({k: ("in_progress", None) for k in wait.WORKFLOWS}),
                    clock=lambda: 1)


if __name__ == "__main__":
    unittest.main()

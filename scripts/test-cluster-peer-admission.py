#!/usr/bin/env python3
import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "cluster_policy", Path(__file__).with_name("cluster-peer-admission.py")
)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


class PeerAdmissionTests(unittest.TestCase):
    def setUp(self):
        self.policy = mod.PeerPolicy("node-b", "a"*64, ("10.12.0.0/24",), 1)
        self.kw = dict(encrypted=True, authenticated=True, operator_enabled=True)

    def check(self, **changes):
        kwargs = self.kw | changes
        return mod.eligible_peer(self.policy, "node-b", "a"*64, "10.12.0.8", 1, **kwargs)

    def test_exact_pinning_and_network(self):
        self.assertTrue(self.check())
        for field in ("encrypted", "authenticated", "operator_enabled"):
            self.assertFalse(self.check(**{field: False}))
        self.assertFalse(mod.eligible_peer(
            self.policy, "wrong", "a"*64, "10.12.0.8", 1, **self.kw))
        self.assertFalse(mod.eligible_peer(
            self.policy, "node-b", "b"*64, "10.12.0.8", 1, **self.kw))
        self.assertFalse(mod.eligible_peer(
            self.policy, "node-b", "a"*64, "127.0.0.1", 1, **self.kw))
        self.assertFalse(mod.eligible_peer(
            self.policy, "node-b", "a"*64, "10.12.0.8", 2, **self.kw))

    def test_revocation_and_malformed_allowlist(self):
        revoked = mod.dataclasses.replace(self.policy, revoked=True)
        self.assertFalse(mod.eligible_peer(revoked, "node-b", "a"*64, "10.12.0.8", 1, **self.kw))
        malformed = mod.dataclasses.replace(self.policy, allowed_networks=("not-a-network",))
        self.assertFalse(mod.eligible_peer(malformed, "node-b", "a"*64, "10.12.0.8", 1, **self.kw))


if __name__ == "__main__":
    unittest.main()

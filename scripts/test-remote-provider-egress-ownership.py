#!/usr/bin/env python3
"""Offline remote provider OS/application egress responsibility regression (#263).

No network traffic or credentials are used; this does not certify real KVM.
"""

import ipaddress
import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


class RemoteProviderIsolationTests(unittest.TestCase):
    def test_expected_os_denials_are_precisely_the_local_network_categories(self):
        config = json.loads(
            (ROOT / "testbed/remote-provider-kvm/network.json").read_text()
        )
        addresses = config["addresses"]
        policy = config["expected_policy"]
        os_policy = config["unit_os_policy"]
        blocked = set(os_policy["blocked"])
        permitted = set(os_policy["not_blocked"])
        self.assertEqual(blocked | permitted, set(addresses))
        self.assertFalse(blocked & permitted)
        self.assertIn("metadata_v4", blocked)
        for name in blocked:
            self.assertEqual(policy[name], "refused")
            ip = ipaddress.ip_address(addresses[name])
            self.assertTrue(ip.is_loopback or ip.is_link_local)

    def test_provider_unit_cannot_inherit_proxy_or_plaintext_key(self):
        unit = (
            ROOT / "distro/templates/remote-inference/flowerly-provider-remote@.service"
        ).read_text()
        self.assertIn("LoadCredentialEncrypted=%i.key:", unit)
        self.assertIn("NoNewPrivileges=yes", unit)
        self.assertIn("ProtectControlGroups=yes", unit)
        self.assertIn("IPAddressDeny=localhost link-local multicast", unit)
        self.assertNotIn("Environment=OPENAI_API_KEY=", unit)
        self.assertNotIn("LoadCredential=%i.key:", unit)
        self.assertIn(
            "HTTPS_PROXY", unit.split("UnsetEnvironment=", 1)[1].splitlines()[0]
        )


if __name__ == "__main__":
    unittest.main()

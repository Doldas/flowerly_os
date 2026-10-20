#!/usr/bin/env python3
"""Negative merge-gate test: proves branch-forward blocks on failed CI.

Tests the verify_master logic with mocked GitHub API responses to verify
that promotion is blocked when:
- CI run fails (via successful_run mock)
- Security check fails (via successful_run mock)
- CodeQL check fails (via successful_run mock)
- Wrong release branch order (via branch_names mock)
"""

import sys
import pathlib
import importlib.util
import os
import unittest

# Load branch_forward.py as a module
spec = importlib.util.spec_from_file_location("branch_forward", pathlib.Path(__file__).parent / "branch-forward.py")
bf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bf)


class TestNegativeMergeGate(unittest.TestCase):
    """Tests that verify_master correctly rejects invalid promotions."""

    def setUp(self):
        """Set required environment variables for branch_forward.api."""
        os.environ["GITHUB_TOKEN"] = "test-token"
        os.environ["GITHUB_REPOSITORY"] = "test/repo"

    def tearDown(self):
        """Clean up environment variables."""
        os.environ.pop("GITHUB_TOKEN", None)
        os.environ.pop("GITHUB_REPOSITORY", None)

    def test_permitted_release_to_master(self):
        """Release branch can promote to master."""
        self.assertTrue(bf.permitted("release/2", "master"))
        self.assertTrue(bf.permitted("release/10", "master"))

    def test_permitted_dev_to_release(self):
        """Dev branches can forward to next release."""
        self.assertTrue(bf.permitted("feature/foo", "release/2"))
        self.assertTrue(bf.permitted("bugfix/bar", "release/3"))
        self.assertTrue(bf.permitted("docs/baz", "release/4"))

    def test_permitted_adjacent_release(self):
        """Adjacent releases can forward."""
        self.assertTrue(bf.permitted("release/1", "release/2"))
        self.assertTrue(bf.permitted("release/2", "release/3"))

    def test_rejected_skip_release(self):
        """Skipping a release is rejected (release-to-release)."""
        self.assertFalse(bf.permitted("release/1", "release/3"))
        # feature/bugfix/docs can target any release (only release-to-release has adjacency requirement)
        self.assertTrue(bf.permitted("feature/x", "release/3"))
        self.assertTrue(bf.permitted("bugfix/x", "release/3"))
        self.assertTrue(bf.permitted("docs/x", "release/3"))

    def test_rejected_master_to_anything(self):
        """Master cannot be base for non-release."""
        self.assertFalse(bf.permitted("feature/x", "master"))
        self.assertFalse(bf.permitted("bugfix/x", "master"))

    def test_rejected_invalid_base(self):
        """Invalid base branches are rejected."""
        self.assertFalse(bf.permitted("feature/x", "develop"))
        self.assertFalse(bf.permitted("release/1", "hotfix/x"))

    def test_version_parsing(self):
        """Version parsing works correctly."""
        self.assertEqual(bf.version("release/1"), 1)
        self.assertEqual(bf.version("release/10"), 10)
        self.assertEqual(bf.version("release/100"), 100)
        self.assertIsNone(bf.version("feature/x"))
        self.assertIsNone(bf.version("master"))
        self.assertIsNone(bf.version("release/0"))

    def test_releases_sorting(self):
        """Releases are sorted numerically."""
        names = ["release/3", "release/1", "release/2", "feature/x", "master"]
        result = bf.releases(names)
        self.assertEqual(result, ["release/1", "release/2", "release/3"])

    def test_releases_duplicate_error(self):
        """Duplicate release numbers raise error."""
        with self.assertRaises(ValueError):
            bf.releases(["release/1", "release/1"])

    def test_releases_gap_error(self):
        """Missing release numbers raise error."""
        with self.assertRaises(ValueError):
            bf.releases(["release/1", "release/3"])

    def _mock_successful_run(self, mock_sr):
        """Helper to mock successful_run in bf module."""
        bf.__dict__['successful_run'] = mock_sr

    def _mock_branch_names(self, names):
        """Helper to mock branch_names in bf module."""
        bf.__dict__['branch_names'] = lambda client: names

    def _mock_api(self, mock_fn):
        """Helper to mock api in bf module and update verify_master default."""
        bf.__dict__['api'] = mock_fn
        # Also update verify_master's default parameter
        if hasattr(bf.verify_master, '__defaults__') and bf.verify_master.__defaults__:
            defaults = list(bf.verify_master.__defaults__)
            # Find and replace the api function in defaults
            for i, d in enumerate(defaults):
                if d is bf.__dict__.get('api', None) or (hasattr(d, '__name__') and d.__name__ == 'api'):
                    defaults[i] = mock_fn
                    break
            bf.verify_master.__defaults__ = tuple(defaults)

    def test_verify_master_rejects_failed_ci(self):
        """Promotion blocked when CI run fails."""
        def mock_sr(workflow, branch, sha, client):
            if workflow == "ci.yml":
                raise ValueError(f"{workflow}: no successful completed PUSH run on {branch}@{sha}")
            return {"id": 123, "head_sha": sha, "head_branch": branch}
        def mock_api(method, path, data=None):
            if path.startswith("/branches/"):
                return {"commit": {"sha": "a" * 40}}
            raise ValueError(f"Unexpected API call: {method} {path}")
        self._mock_branch_names(["release/1", "release/2"])
        self._mock_successful_run(mock_sr)
        self._mock_api(mock_api)

        with self.assertRaises(ValueError) as cm:
            bf.verify_master("release/2", "a" * 40)
        self.assertIn("ci.yml: no successful completed PUSH run", str(cm.exception))

    def test_verify_master_rejects_failed_security(self):
        """Promotion blocked when security check fails."""
        def mock_sr(workflow, branch, sha, client):
            if workflow == "ci.yml":
                return {"id": 123, "head_sha": sha, "head_branch": branch}
            raise ValueError(f"{workflow}: no successful completed PUSH run on {branch}@{sha}")
        def mock_api(method, path, data=None):
            if path.startswith("/branches/"):
                return {"commit": {"sha": "a" * 40}}
            raise ValueError(f"Unexpected API call: {method} {path}")
        self._mock_branch_names(["release/1", "release/2"])
        self._mock_successful_run(mock_sr)
        self._mock_api(mock_api)

        with self.assertRaises(ValueError) as cm:
            bf.verify_master("release/2", "a" * 40)
        self.assertIn("security.yml: no successful completed PUSH run", str(cm.exception))

    def test_verify_master_rejects_failed_codeql(self):
        """Promotion blocked when CodeQL fails."""
        def mock_sr(workflow, branch, sha, client):
            if workflow in ("ci.yml", "security.yml"):
                return {"id": 123, "head_sha": sha, "head_branch": branch}
            raise ValueError(f"{workflow}: no successful completed PUSH run on {branch}@{sha}")
        def mock_api(method, path, data=None):
            if path.startswith("/branches/"):
                return {"commit": {"sha": "a" * 40}}
            raise ValueError(f"Unexpected API call: {method} {path}")
        self._mock_branch_names(["release/1", "release/2"])
        self._mock_successful_run(mock_sr)
        self._mock_api(mock_api)

        with self.assertRaises(ValueError) as cm:
            bf.verify_master("release/2", "a" * 40)
        self.assertIn("codeql.yml: no successful completed PUSH run", str(cm.exception))

    def test_verify_master_rejects_wrong_release_order(self):
        """Promotion blocked when trying to promote non-latest release."""
        self._mock_branch_names(["release/1", "release/2", "release/3"])
        def mock_api(method, path, data=None):
            if path.startswith("/branches/"):
                return {"commit": {"sha": "a" * 40}}
            raise ValueError(f"Unexpected API call: {method} {path}")
        self._mock_api(mock_api)

        with self.assertRaises(ValueError) as cm:
            bf.verify_master("release/2", "a" * 40)
        self.assertIn("only latest release release/3 can enter master", str(cm.exception))


if __name__ == "__main__":
    unittest.main(verbosity=2)
#!/usr/bin/env python3
"""Offline failure-injection tests for protected branch forward merges."""
import importlib.util
import os
from pathlib import Path
import unittest
from unittest.mock import patch

path = Path(__file__).with_name("branch-forward.py")
spec = importlib.util.spec_from_file_location("branch_forward", path)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)
SHA = "a" * 40
OLD = "b" * 40


def fake(releases=("release/2", "release/3"), *, arch="success",
         ci="success", security="success", codeql="success", stale=False,
         auto=True, existing=False):
    calls = []
    def client(method, path, data=None):
        calls.append((method, path, data))
        if path == "":
            return {"allow_auto_merge": auto}
        if path.startswith("/branches?"):
            return [{"name": name} for name in releases]
        if path.startswith("/branches/release%2F"):
            return {"commit": {"sha": OLD if stale else SHA}}
        if path.startswith("/actions/workflows/"):
            workflow = path.split("/")[3]
            conclusion = (ci if workflow == "ci.yml" else
                          security if workflow == "security.yml" else codeql)
            return {"workflow_runs": [
                {"id": 42, "head_sha": SHA, "head_branch": releases[-1],
                 "event": "push", "status": "completed", "conclusion": conclusion},
                {"id": 43, "head_sha": OLD, "head_branch": releases[-1],
                 "event": "push", "status": "completed", "conclusion": "success"},
            ]}
        if path.startswith("/actions/runs/42/jobs"):
            return {"jobs": [
                {"name": "rust", "conclusion": "success",
                 "steps": [{"name": policy.ARCH, "status": "completed",
                            "conclusion": arch}]},
                {"name": "docs", "conclusion": "success"},
            ]}
        if path.startswith("/compare/"):
            return {"ahead_by": 1, "files": [{"filename": "a.txt"}]}
        if path.startswith("/pulls?"):
            return ([{"number": 99, "node_id": "existing", "draft": False,
                      "auto_merge": {"merge_method": "merge"}}] if existing else [])
        if method == "POST" and path == "/pulls":
            return {"number": 100, "node_id": "new-id", "draft": data["draft"]}
        raise AssertionError((method, path, data))
    return client, calls


class Tests(unittest.TestCase):
    def test_numeric_order_and_gaps(self):
        self.assertEqual(policy.releases(["release/10", "docs/x", "release/9"]),
                         ["release/9", "release/10"])
        with self.assertRaisesRegex(ValueError, "missing release/3"):
            policy.releases(["release/2", "release/4"])
        with self.assertRaisesRegex(ValueError, "duplicate"):
            policy.releases(["release/2", "release/2"])
        self.assertIsNone(policy.version("release/02"))

    def test_direction_guards(self):
        for head, base in [("release/2", "release/3"), ("release/3", "master"),
                           ("feature/x", "release/2"), ("bugfix/x", "release/3"),
                           ("docs/x", "release/2")]:
            self.assertTrue(policy.permitted(head, base))
        for head, base in [("release/3", "release/2"), ("release/2", "release/4"),
                           ("master", "release/3"), ("feature/x", "master"),
                           ("release/2", "release/2")]:
            self.assertFalse(policy.permitted(head, base))

    def test_verification_must_be_exact_sha_latest_and_real_arch(self):
        client, _ = fake()
        self.assertTrue(policy.verify_master("release/3", SHA, client))
        for changed in ({"arch": "skipped"}, {"ci": "failure"},
                        {"security": "failure"}, {"codeql": "failure"},
                        {"stale": True}):
            with self.subTest(changed=changed):
                with self.assertRaises(ValueError):
                    policy.verify_master("release/3", SHA, fake(**changed)[0])
        with self.assertRaisesRegex(ValueError, "latest"):
            policy.verify_master("release/2", SHA, client)
        with self.assertRaisesRegex(ValueError, "exact"):
            policy.verify_master("release/3", SHA[:20], client)

    def test_reconcile_only_auto_merges_adjacent_releases(self):
        client, calls = fake()
        graph = []
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "Doldas/flowerly_os"}):
            policy.reconcile(client, lambda *args: graph.append(args))
        made = [data for method, path, data in calls if method == "POST"]
        self.assertEqual([(p["head"], p["base"], p["draft"]) for p in made],
                         [("release/2", "release/3", False),
                          ("release/3", "master", True)])
        self.assertEqual(len(graph), 1)
        self.assertIn("mergeMethod:MERGE", graph[0][0])

    def test_missing_auto_merge_setting_leaves_safe_pr(self):
        client, calls = fake(auto=False)
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "Doldas/flowerly_os"}):
            policy.reconcile(client, lambda *_: self.fail("auto merge prohibited"))
        self.assertEqual(sum(method == "POST" for method, _, _ in calls), 2)

    def test_existing_pr_never_duplicated(self):
        client, calls = fake(existing=True)
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": "Doldas/flowerly_os"}):
            policy.ensure_pr("release/2", "release/3", False, True, client,
                             lambda *_: self.fail("already auto-merged"))
        self.assertFalse(any(method == "POST" for method, _, _ in calls))

    def test_no_backward_or_feature_master_promotion(self):
        with self.assertRaisesRegex(ValueError, "forbidden"):
            policy.ensure_pr("feature/x", "master", True, False, lambda *_: None)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Regression tests for the workflow token permission policy (#187)."""
import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).with_name("test-workflow-permissions.py")
spec = importlib.util.spec_from_file_location("workflow_permissions", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

READ_ONLY = "on: push\npermissions:\n  contents: read\njobs:\n  build:\n    runs-on: x\n"


class WorkflowPermissionTests(unittest.TestCase):
    def check(self, text, name="ci.yml"):
        return module.problems_of(name, text)

    def test_repository_workflows_pass(self):
        self.assertEqual(module.main(), 0)

    def test_read_only_top_level_passes(self):
        self.assertEqual(self.check(READ_ONLY), [])

    def test_empty_and_read_all_top_level_pass(self):
        for value in ("{}", "read-all", "{}  # none"):
            self.assertEqual(self.check(f"permissions: {value}\njobs:\n  a:\n    runs-on: x\n"),
                             [], value)

    def test_missing_top_level_block_is_rejected(self):
        self.assertTrue(self.check("on: push\njobs:\n  a:\n    runs-on: x\n"))

    def test_job_level_block_alone_is_not_top_level(self):
        text = "jobs:\n  a:\n    permissions:\n      contents: read\n"
        self.assertTrue(self.check(text))

    def test_commented_block_is_not_a_declaration(self):
        self.assertTrue(self.check("# permissions:\n#   contents: read\njobs:\n  a:\n    runs-on: x\n"))

    def test_top_level_write_is_rejected_even_if_allowed_for_a_job(self):
        text = "permissions:\n  contents: read\n  pages: write\njobs:\n  deploy:\n    runs-on: x\n"
        self.assertTrue(self.check(text, "docs-pages.yml"))

    def test_write_all_is_rejected(self):
        self.assertTrue(self.check("permissions: write-all\njobs:\n  a:\n    runs-on: x\n"))

    def test_inline_mapping_fails_closed(self):
        self.assertTrue(self.check("permissions: { contents: write }\njobs:\n  a:\n    runs-on: x\n"))

    def test_unlisted_job_write_is_rejected(self):
        text = READ_ONLY + "    permissions:\n      issues: write\n"
        self.assertTrue(self.check(text))

    def test_listed_job_write_passes_only_for_that_job(self):
        allowed = "permissions:\n  contents: read\njobs:\n  release:\n    permissions:\n      contents: write\n"
        self.assertEqual(self.check(allowed, "release.yml"), [])
        self.assertTrue(self.check(allowed.replace("release:", "build:"), "release.yml"))
        self.assertTrue(self.check(allowed, "ci.yml"))

    def test_unknown_level_is_rejected(self):
        self.assertTrue(self.check("permissions:\n  contents: admin\njobs:\n  a:\n    runs-on: x\n"))

    def test_trailing_comments_are_ignored(self):
        text = "permissions:  # least privilege\n  contents: read  # checkout\njobs:\n  a:\n    runs-on: x\n"
        self.assertEqual(self.check(text), [])


if __name__ == "__main__":
    unittest.main()

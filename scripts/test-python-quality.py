#!/usr/bin/env python3
"""Offline regression of the strict, incremental Python quality gate."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
GATE = ROOT / "check-python-quality.py"


def command(*argv, cwd=None, env=None, check=True):
    return subprocess.run(
        argv, cwd=cwd, env=env, check=check, text=True, capture_output=True
    )


class PythonQualityTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        command("git", "init", "-q", str(self.root))
        command("git", "config", "user.name", "Fixture", cwd=self.root)
        command("git", "config", "user.email", "fixture@example.invalid", cwd=self.root)
        (self.root / "unchanged.py").write_text("n = 1\n")
        (self.root / "existing.py").write_text("x = 2\n")
        command("git", "add", ".", cwd=self.root)
        command("git", "commit", "-qm", "base", cwd=self.root)
        self.base = command("git", "rev-parse", "HEAD", cwd=self.root).stdout.strip()
        (self.root / "existing.py").write_text("x = 3\n")
        (self.root / "new file.py").write_text("y = 4\n")
        command("git", "add", ".", cwd=self.root)
        command("git", "commit", "-qm", "add", cwd=self.root)

        self.log = self.root / "ruff-log.jsonl"
        self.ruff = self.root / "fake-ruff"
        self.ruff.write_text(
            "#!/usr/bin/env python3\n"
            "import json, os, sys\n"
            "with open(os.environ['RUFF_TEST_LOG'], 'a') as sink:\n"
            "    sink.write(json.dumps(sys.argv[1:]) + '\\n')\n"
        )
        self.ruff.chmod(0o755)
        self.env = dict(os.environ, RUFF_TEST_LOG=str(self.log))

    def gate(self, *args):
        return command(
            sys.executable,
            str(GATE),
            "--root",
            str(self.root),
            "--ruff",
            str(self.ruff),
            *args,
            env=self.env,
            check=False,
        )

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(row) for row in self.log.read_text().splitlines()]

    def test_only_changed_linted_and_new_files_formatted(self):
        result = self.gate("--base", self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.calls(),
            [
                ["check", "--no-cache", "--", "existing.py", "new file.py"],
                ["format", "--check", "--", "new file.py"],
            ],
        )

    def test_invalid_base_fails_closed(self):
        for wrong in ("0" * 40, "not-an-object"):
            self.log.unlink(missing_ok=True)
            result = self.gate("--base", wrong)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(self.calls(), [])

    def test_unchanged_syntax_error_cannot_hide(self):
        (self.root / "unchanged.py").write_text("if True print('broken')\n")
        result = self.gate("--base", self.base)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unchanged.py", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_no_base_does_not_silently_succeed(self):
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_ruff_failure_propagates(self):
        self.ruff.write_text("#!/usr/bin/env python3\nimport sys\nsys.exit(7)\n")
        result = self.gate("--base", self.base)
        self.assertNotEqual(result.returncode, 0)

    def test_all_mode_lints_all_without_reformatting_legacy(self):
        result = self.gate("--all")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.calls(),
            [
                [
                    "check",
                    "--no-cache",
                    "--",
                    "existing.py",
                    "new file.py",
                    "unchanged.py",
                ]
            ],
        )


if __name__ == "__main__":
    unittest.main()

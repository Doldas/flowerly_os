#!/usr/bin/env python3
"""Reject new raw PTY/cgroup process authority in high-level domains (#95).

Source-level guard only; privileged Arch/PTY adversarial tests remain required.
"""
from pathlib import Path
import re
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
DOMAINS = ("flow", "goal", "generation", "provenance", "agentic",
           "ownership", "capability")
# These domains do not own raw PID-based authority. Trusted session/IPC
# supervisors and the reviewed Linux compute host adapter remain separate.
FORBIDDEN = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+(?:::)?"
    r"(?:std|tokio)::process\b|"
    r"(?m:(?:/sys/fs/cgroup/|cgroup\.procs|cgroup\.kill))|"\n    r"(?m:(?:std|tokio)::process::Command::new\\s*\\()"
)


def violations(root=ROOT):
    base = root / "src/flowerlyd/src"
    paths = [path for name in DOMAINS for path in (base / name).rglob("*.rs")]
    paths.append(base / "authorization.rs")
    errors = []
    for path in sorted(p for p in paths if p.is_file()):
        text = path.read_text(encoding="utf-8")
        if FORBIDDEN.search(text):
            errors.append(path.relative_to(root).as_posix())
    return errors


class ProcessAuthorityBoundaryTests(unittest.TestCase):
    def test_current_domains_do_not_gain_direct_process_authority(self):
        self.assertEqual(violations(), [])

    def test_raw_process_imports_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            file = root / "src/flowerlyd/src/flow/mod.rs"
            file.parent.mkdir(parents=True)
            for text in (
                "use std::process::Command;\n",
                "pub use tokio::process::Command;\n",
                "const C: &str = \"/sys/fs/cgroup/cgroup.kill\";\n",
                "const F: &str = \"cgroup.procs\";\n",
                "let cmd = std::process::Command::new(\"sh\");\n",
            ):
                with self.subTest(source=text):
                    file.write_text(text)
                    self.assertEqual(len(violations(root)), 1)


if __name__ == "__main__":
    unittest.main()

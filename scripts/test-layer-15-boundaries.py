#!/usr/bin/env python3
"""Fail closed on newly introduced direct driver imports outside reviewed adapters.

This is a deliberately narrow migration gate: existing SQLite and Linux
security-critical code is explicitly listed, not blindly moved or weakened.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "src"
# Existing ownership reviewed under #91. Changes to this list need security review.
SQLITE_OWNERS = {"src/shared/flowerly-core/src/storage/mod.rs"}
LINUX_OWNERS = {
    "src/shared/flowerly-core/src/ipc.rs",
    "src/shared/flowerly-core/src/daemon.rs",
    "src/flowerlyd/src/session/mod.rs",
    "src/flowerlyd/src/session/pty.rs",
    "src/flowerlyd/src/session/runtime.rs",
    "src/evolutiond/src/kvm/mod.rs",
    "src/flowerlyd/src/compute/host.rs",
}
SQLITE_IMPORT = re.compile(
    r"(?m)^\s*(?:(?:pub(?:\([^)]*\))?\s+)?use\s+(?:::)?rusqlite\b"
    r"(?:::|\s*;|\s+as\b)|extern\s+crate\s+rusqlite\b)"
)
# Only flag explicit Linux sysfs/procfs references and direct Unix OS interface imports.
HOST_ACCESS = re.compile(
    r'(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\s+(?:::)?std::os(?:::unix\b|::\{\s*unix\b)|'
    r'(?m:(?:/proc/|/sys/fs/cgroup/))'
)


def unauthorized_imports(root=ROOT):
    violations = []
    # Start with higher-level Flowerly command modules only. The transport,
    # PTY supervisor, provider process and SQLite adapter retain reviewed
    # exceptions until dedicated migration and kernel-negative tests land.
    higher_level = root / "src/flowerlyd/src"
    domains = ("flow", "goal", "generation", "provenance", "agentic", "compute")
    for path in sorted(p for domain in domains
                       for p in (higher_level / domain).rglob("*.rs")):
        relative = path.relative_to(root).as_posix()
        source = path.read_text(encoding="utf-8")
        if SQLITE_IMPORT.search(source) and relative not in SQLITE_OWNERS:
            violations.append(f"{relative}: raw SQLite import outside storage adapter")
        if HOST_ACCESS.search(source) and relative not in LINUX_OWNERS:
            violations.append(f"{relative}: raw host access outside reviewed boundary")
    return violations


class DependencyBoundaryTests(unittest.TestCase):
    def test_existing_dependency_boundary(self):
        self.assertEqual(unauthorized_imports(), [])

    def test_new_sqlite_import_is_rejected(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "src/flowerlyd/src/goal/mod.rs"
            file.parent.mkdir(parents=True)
            file.write_text("use rusqlite::Connection;\n")
            self.assertIn("raw SQLite", unauthorized_imports(root)[0])

    def test_sqlite_aliases_and_absolute_paths_cannot_bypass_boundary(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "src/flowerlyd/src/goal/mod.rs"
            file.parent.mkdir(parents=True)
            for declaration in (
                "use rusqlite as db;\n",
                "use ::rusqlite::Connection;\n",
                "pub use rusqlite::Connection;\n",
                "pub(crate) use rusqlite as db;\n",
                "extern crate rusqlite;\n",
            ):
                with self.subTest(declaration=declaration):
                    file.write_text(declaration)
                    self.assertTrue(
                        any("raw SQLite" in entry for entry in unauthorized_imports(root))
                    )

    def test_unix_import_variants_cannot_bypass_boundary(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "src/flowerlyd/src/flow/mod.rs"
            file.parent.mkdir(parents=True)
            for declaration in (
                "use std::os::unix::fs::PermissionsExt;\n",
                "pub use std::os::unix::net::UnixStream;\n",
                "use ::std::os::unix::io::RawFd;\n",
                "use std::os::{unix::fs::PermissionsExt};\n",
            ):
                with self.subTest(declaration=declaration):
                    file.write_text(declaration)
                    self.assertTrue(
                        any("raw host access" in entry for entry in unauthorized_imports(root))
                    )

    def test_new_unreviewed_kernel_access_is_rejected(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "src/flowerlyd/src/flow/mod.rs"
            file.parent.mkdir(parents=True)
            file.write_text('const CGROUP: &str = "/sys/fs/cgroup/cgroup.kill";\n')
            self.assertIn("raw host access", unauthorized_imports(root)[0])


if __name__ == "__main__":
    unittest.main()

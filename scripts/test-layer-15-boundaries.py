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
# Raw kernel/FFI crates must not be introduced into domain modules either.
KERNEL_CRATE_IMPORT = re.compile(
    r"(?m)^\s*(?:(?:pub(?:\([^)]*\))?\s+)?use\s+(?:::)?(?:libc|nix|rustix)\b"
    r"(?:::|\s*;|\s+as\b)|extern\s+crate\s+(?:libc|nix|rustix)\b)"
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
    domains = ("flow", "goal", "generation", "provenance", "agentic",
               "compute", "ownership", "capability")
    paths = [p for domain in domains
             for p in (higher_level / domain).rglob("*.rs")]
    # Authorization lives at crate root, not under a domain directory.
    paths.extend((higher_level / "authorization.rs",))
    for path in sorted(p for p in paths if p.is_file()):
        relative = path.relative_to(root).as_posix()
        source = path.read_text(encoding="utf-8")
        # Existing isolated Rust unit-test fixtures may construct SQLite
        # error codes. A direct runtime driver reference remains prohibited.
        runtime_source = source.split("#[cfg(test)]", 1)[0]
        if (SQLITE_IMPORT.search(source) or re.search(r"\brusqlite::", runtime_source)) and relative not in SQLITE_OWNERS:
            violations.append(f"{relative}: raw SQLite import outside storage adapter")
        if KERNEL_CRATE_IMPORT.search(source) and relative not in LINUX_OWNERS:
            violations.append(f"{relative}: direct kernel/FFI crate import outside reviewed boundary")
        if HOST_ACCESS.search(source) and relative not in LINUX_OWNERS:
            violations.append(f"{relative}: raw host access outside reviewed boundary")
    return violations


class DependencyBoundaryTests(unittest.TestCase):
    def test_existing_dependency_boundary(self):
        self.assertEqual(unauthorized_imports(), [])

    def test_direct_sqlite_paths_outside_imports_are_forbidden(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "src/flowerlyd/src/goal/mod.rs"
            path.parent.mkdir(parents=True)
            path.write_text("let conn = rusqlite::Connection::open(path);\n")
            self.assertTrue(any("SQLite" in error for error in unauthorized_imports(root)))

    def test_test_only_sqlite_error_fixture_is_not_runtime_driver(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "src/flowerlyd/src/generation/mod.rs"
            path.parent.mkdir(parents=True)
            path.write_text('#[cfg(test)]\\nmod tests { fn fake() { let error = rusqlite::Error::InvalidQuery; } }\\n')
            self.assertEqual(unauthorized_imports(root), [])

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

    def test_ownership_and_capability_are_guarded(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            for domain in ("ownership", "capability"):
                file = root / f"src/flowerlyd/src/{domain}/mod.rs"
                file.parent.mkdir(parents=True)
                file.write_text("use rusqlite::Connection;\n")
                with self.subTest(domain=domain):
                    self.assertTrue(
                        any("raw SQLite" in entry for entry in unauthorized_imports(root))
                    )
                file.write_text("")

    def test_root_authorization_module_is_guarded(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            file = root / "src/flowerlyd/src/authorization.rs"
            file.parent.mkdir(parents=True)
            file.write_text("use std::os::unix::fs::PermissionsExt;\n")
            self.assertTrue(
                any("raw host access" in entry for entry in unauthorized_imports(root))
            )

    def test_kernel_crate_imports_cannot_bypass_boundary(self):
        from tempfile import TemporaryDirectory
        with TemporaryDirectory() as temp:
            root = Path(temp)
            path = root / "src/flowerlyd/src/goal/mod.rs"
            path.parent.mkdir(parents=True)
            for import_line in (
                "use libc::pid_t;\n",
                "pub use ::nix::unistd;\n",
                "extern crate rustix;\n",
                "use rustix as system;\n",
            ):
                with self.subTest(import_line=import_line):
                    path.write_text(import_line)
                    self.assertTrue(
                        any("kernel/FFI crate" in item for item in unauthorized_imports(root))
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

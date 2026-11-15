#!/usr/bin/env python3
"""Schema reader window against a real older binary (ADR-0023 section 5, R4).

`min_reader` is only a promise until an older binary is run on a newer
database. This builds `flowerlyd` from the commit recorded in
testbed/schema-compat/baseline.json and runs real processes:

1. the new binary creates a database and writes; the baseline binary then
   starts on it: it must work, or, for a migration listed as irreversible
   since the baseline, refuse without changing a byte of the data;
2. the baseline creates a database and writes; the new binary upgrades it,
   keeps the data and works;
3. the new binary is the older reader of a synthetic newer schema: an
   expand-only migration inside its window is accepted, a raised
   `min_reader` is refused without changes.

Every migration newer than the baseline must either stay readable by it or be
listed as irreversible: an unlisted one fails this check (the compatibility
entry R4 requires). `--self-test` checks that rule offline.

Usage: test-schema-compat.py BIN_DIR | --self-test
"""

import json
import os
import pathlib
import re
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASELINE = ROOT / "testbed/schema-compat/baseline.json"
MIGRATIONS_RS = ROOT / "src/shared/flowerly-core/src/storage/mod.rs"
REFUSAL = "is newer than this binary supports"


def declared_migrations(source):
    """(version, min_reader) of each entry in MIGRATIONS, in order."""
    found = [
        (int(v), int(m))
        for v, m in re.findall(
            r"Migration \{\s*version: (\d+),\s*min_reader: (\d+),", source
        )
    ]
    entries = source.count("Migration {\n        version:")
    if not found or len(found) != entries:
        raise SystemExit("every migration must declare min_reader right after version")
    return found


def compat_problems(migrations, baseline):
    """Migrations newer than the baseline with no compatibility entry."""
    problems = []
    supported = baseline["schema_version"]
    irreversible = set(baseline.get("irreversible", []))
    known = {version for version, _ in migrations}
    for version, min_reader in migrations:
        if version <= supported:
            continue
        if min_reader > supported and version not in irreversible:
            problems.append(
                f"migration {version} needs min_reader {min_reader} > baseline "
                f"{supported}: list it as irreversible or keep the window"
            )
        if min_reader <= supported and version in irreversible:
            problems.append(
                f"migration {version} is listed irreversible but is readable"
            )
    for version in sorted(irreversible - known):
        problems.append(f"irreversible migration {version} does not exist")
    return problems


def self_test():
    baseline = {"schema_version": 15, "irreversible": [16]}
    assert compat_problems([(15, 15), (16, 16)], baseline) == []
    assert compat_problems([(15, 15), (16, 16), (17, 15)], baseline) == []
    assert compat_problems([(16, 16), (17, 17)], baseline), "unlisted raise accepted"
    assert compat_problems(
        [(16, 16), (17, 15)], {"schema_version": 15, "irreversible": [16, 17]}
    )
    assert compat_problems([(16, 16)], {"schema_version": 15, "irreversible": [16, 18]})
    sample = (
        'Migration {\n        version: 1,\n        min_reader: 1,\n        name: "a"'
    )
    assert declared_migrations(sample) == [(1, 1)]
    missing = 'Migration {\n        version: 1,\n        name: "a"'
    try:
        declared_migrations(missing)
    except SystemExit:
        pass
    else:
        raise AssertionError("a migration without min_reader was accepted")
    current = compat_problems(
        declared_migrations(MIGRATIONS_RS.read_text()), json.loads(BASELINE.read_text())
    )
    assert current == [], current
    print("schema compatibility entries: every migration since the baseline is covered")


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise AssertionError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def call(sock, name, args):
    message = json.dumps(
        {
            "version": 1,
            "id": str(uuid.uuid4()),
            "type": "command",
            "name": name,
            "args": args,
        }
    ).encode()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(sock))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        return json.loads(recv_exact(client, size))


def ok(sock, name, args):
    reply = call(sock, name, args)
    assert reply["type"] == "ok", (name, reply)
    return reply["data"]


class Daemon:
    def __init__(self, binary, root):
        self.binary = binary
        self.state = root / "state"
        self.runtime = root / "runtime"
        self.log = root / f"{binary.parent.name}-{uuid.uuid4().hex[:6]}.log"
        self.state.mkdir(mode=0o700, exist_ok=True)
        self.runtime.mkdir(mode=0o700, exist_ok=True)
        self.sock = self.runtime / "flowerlyd.sock"
        env = dict(
            os.environ,
            AGENTIC_RUNTIME_DIR=str(self.runtime),
            AGENTIC_STATE_DIR=str(self.state),
        )
        env.pop("AGENTIC_DATABASE_PATH", None)
        with self.log.open("wb") as log:
            self.process = subprocess.Popen(
                [str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT
            )

    def ready(self):
        for _ in range(200):
            if self.process.poll() is not None:
                return False
            if self.sock.exists():
                try:
                    if call(self.sock, "flow.list", {})["type"] == "ok":
                        return True
                except (OSError, AssertionError, TimeoutError):
                    pass
            time.sleep(0.05)
        raise AssertionError(f"daemon neither ready nor exited: {self.log.read_text()}")

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            self.process.wait(timeout=20)
        if self.sock.exists():
            self.sock.unlink()

    def text(self):
        return self.log.read_text(errors="replace")


def snapshot(state):
    """Logical content of the database, independent of WAL file layout."""
    db = sqlite3.connect(f"file:{state / 'state.db'}?mode=ro", uri=True)
    try:
        return list(db.iterdump())
    finally:
        db.close()


def build_baseline(commit):
    out = ROOT / "target/schema-compat" / commit
    binary = out / "target/debug/flowerlyd"
    if binary.is_file():
        return binary
    present = subprocess.run(
        ["git", "-C", str(ROOT), "cat-file", "-e", f"{commit}^{{commit}}"],
        capture_output=True,
    )
    if present.returncode != 0:
        subprocess.run(
            [
                "git",
                "-C",
                str(ROOT),
                "fetch",
                "--no-tags",
                "--depth",
                "1",
                "origin",
                commit,
            ],
            check=True,
        )
    source = out / "src"
    source.mkdir(parents=True, exist_ok=True)
    archive = subprocess.run(
        ["git", "-C", str(ROOT), "archive", "--format=tar", commit],
        check=True,
        capture_output=True,
    ).stdout
    with tempfile.TemporaryFile() as handle:
        handle.write(archive)
        handle.seek(0)
        with tarfile.open(fileobj=handle) as tar:
            tar.extractall(source, filter="data")
    subprocess.run(
        ["cargo", "build", "--locked", "-p", "flowerlyd"],
        cwd=source,
        env=dict(os.environ, CARGO_TARGET_DIR=str(out / "target")),
        check=True,
    )
    return binary


def expect_refusal(binary, root, before, what):
    daemon = Daemon(binary, root)
    try:
        assert not daemon.ready(), f"{what}: started on an unreadable schema"
        assert daemon.process.returncode != 0, what
        assert REFUSAL in daemon.text(), f"{what}: {daemon.text()}"
    finally:
        daemon.stop()
    assert snapshot(root / "state") == before, f"{what}: the database changed"


def run(bin_dir):
    baseline = json.loads(BASELINE.read_text())
    migrations = declared_migrations(MIGRATIONS_RS.read_text())
    problems = compat_problems(migrations, baseline)
    if problems:
        raise SystemExit("\n".join(problems))
    newest = max(version for version, _ in migrations)
    window = max(m for v, m in migrations if v >= 16)
    irreversible = [
        v for v in baseline.get("irreversible", []) if v > baseline["schema_version"]
    ]
    new = bin_dir / "flowerlyd"
    old = build_baseline(baseline["commit"])

    with tempfile.TemporaryDirectory(prefix="flowerly-compat-") as work:
        work = pathlib.Path(work)

        # 1. The baseline on a database the new binary created.
        root = work / "new-then-old"
        root.mkdir()
        daemon = Daemon(new, root)
        assert daemon.ready(), daemon.text()
        ok(daemon.sock, "flow.create", {"name": "compat-new"})
        ok(daemon.sock, "generation.list", {})
        daemon.stop()
        before = snapshot(root / "state")
        if irreversible:
            expect_refusal(
                old, root, before, f"baseline after irreversible {irreversible}"
            )
        else:
            daemon = Daemon(old, root)
            assert daemon.ready(), daemon.text()
            ok(daemon.sock, "generation.list", {})
            ok(daemon.sock, "flow.create", {"name": "compat-old-write"})
            daemon.stop()
        print(
            f"older binary on the new schema: {'refused unchanged' if irreversible else 'works'}"
        )

        # 2. The new binary upgrades a database the baseline created.
        root = work / "old-then-new"
        root.mkdir()
        daemon = Daemon(old, root)
        assert daemon.ready(), daemon.text()
        ok(daemon.sock, "flow.create", {"name": "compat-old"})
        daemon.stop()
        daemon = Daemon(new, root)
        assert daemon.ready(), daemon.text()
        names = {flow["name"] for flow in ok(daemon.sock, "flow.list", {})["flows"]}
        assert "compat-old" in names, names
        ok(daemon.sock, "flow.create", {"name": "compat-upgraded"})
        ok(daemon.sock, "generation.list", {})
        daemon.stop()
        db = sqlite3.connect(root / "state/state.db")
        version, min_reader = db.execute(
            "SELECT (SELECT MAX(version) FROM schema_migrations), min_reader FROM schema_compat"
        ).fetchone()
        db.close()
        assert (version, min_reader) == (newest, window), (version, min_reader)
        print("new binary upgrades the baseline's database and keeps its data")

        # 3. The new binary as the older reader of a synthetic newer schema.
        root = work / "window"
        root.mkdir()
        daemon = Daemon(new, root)
        assert daemon.ready(), daemon.text()
        daemon.stop()
        db = sqlite3.connect(root / "state/state.db")
        db.executescript(
            f"""CREATE TABLE future_things (id INTEGER PRIMARY KEY);
            INSERT INTO schema_migrations (version, name, applied_at)
            VALUES ({newest + 1}, 'future_expand', 0);"""
        )
        db.commit()
        db.close()
        daemon = Daemon(new, root)
        assert daemon.ready(), f"expand-only newer schema refused: {daemon.text()}"
        ok(daemon.sock, "flow.create", {"name": "inside-window"})
        daemon.stop()
        db = sqlite3.connect(root / "state/state.db")
        db.execute("UPDATE schema_compat SET min_reader = ?", (newest + 1,))
        db.commit()
        db.close()
        expect_refusal(new, root, snapshot(root / "state"), "raised min_reader")
        print(
            "new binary accepts a newer schema inside its window and refuses one outside"
        )


def main():
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    run(pathlib.Path(sys.argv[1]).resolve())
    print("schema reader window verified with real older and newer binaries")


if __name__ == "__main__":
    main()

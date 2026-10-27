#!/usr/bin/env python3
"""#74 crash/restart matrix for the real flowerlyd with delegated cgroup v2.

Runs as the PTY manager account inside a Delegate=yes unit (see
arch-pty-cgroup-acceptance.sh --system ... BINARY_DIR), with the shell
dropped to a distinct UID. Every case observes the kernel: process
identities (PID + start time), /proc/<pid>/cgroup and the delegated
cgroup tree. Nothing is simulated.

  1. flowerlyd SIGKILL with live keyed and unkeyed Sessions, each with a
     setsid descendant: everything exits, no PTY cgroup remains; after
     restart the Sessions are suspended/detached, the key reports an unknown
     outcome and a retry under it launches nothing; changed arguments
     conflict; a fresh spawn works.
  2. SIGKILL between guardian launch and the Session row (the row insert is
     held behind a real SQLite write lock): keyed and unkeyed.
  3. Total supervision loss: watcher, guardian and flowerlyd SIGKILLed
     together so only a setsid descendant survives in its cgroup. Restart
     must kill it, with or without a Session row or key.
  4. Concurrent spawn (duplicate keys), terminate and restart: one launch
     per key, nothing running after restart, no leaked cgroups.
"""
import base64
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid

PTY_GROUP = re.compile(r"pty-session-[0-9a-f]{32}")


def request(path, name, arguments, timeout=12):
    identifier = str(uuid.uuid4())
    payload = json.dumps({"version": 1, "id": identifier, "type": "command",
                          "name": name, "args": arguments}).encode()
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(path))
        connection.sendall(struct.pack("<I", len(payload)) + payload)
        header = connection.recv(4, socket.MSG_WAITALL)
        assert len(header) == 4, "flowerlyd IPC response truncated"
        body = connection.recv(struct.unpack("<I", header)[0], socket.MSG_WAITALL)
    result = json.loads(body)
    assert result["id"] == identifier, result
    return result


def background_spawn(path, arguments, reply):
    def run():
        try:
            reply.update(request(path, "session.spawn", arguments, timeout=30))
        except (OSError, AssertionError, ValueError):
            reply["type"] = "lost"
    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    return thread


def ok(path, name, arguments):
    result = request(path, name, arguments)
    assert result["type"] == "ok", (name, result)
    return result["data"]


def identity(pid):
    """(state, start time) or None: PID reuse cannot fake a live identity."""
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    except (FileNotFoundError, ProcessLookupError):
        return None
    return fields[0], fields[19]


def alive(pid, original):
    current = identity(pid)
    return current is not None and current[1] == original[1] and current[0] not in "ZX"


def wait_gone(processes, label, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        survivors = [pid for pid, original in processes.items() if alive(pid, original)]
        if not survivors:
            return
        time.sleep(0.02)
    raise AssertionError(f"{label}: processes survived: {survivors}")


def children(pid):
    found = []
    for task in Path(f"/proc/{pid}/task").glob("*"):
        try:
            found += [int(child) for child in (task / "children").read_text().split()]
        except FileNotFoundError:
            pass
    return found


class Daemon:
    def __init__(self, binary_directory, root):
        self.binary = binary_directory / "flowerlyd"
        self.temporary = tempfile.TemporaryDirectory(prefix="flowerly-pty-recovery-")
        base = Path(self.temporary.name)
        self.runtime, self.state = base / "runtime", base / "state"
        self.runtime.mkdir(mode=0o700)
        self.state.mkdir(mode=0o700)
        self.log = base / "flowerlyd.log"
        self.socket = self.runtime / "flowerlyd.sock"
        self.database = self.state / "state.db"
        self.root = root
        self.environment = dict(os.environ, AGENTIC_RUNTIME_DIR=str(self.runtime),
                                AGENTIC_STATE_DIR=str(self.state),
                                FLOWERLY_PTY_ISOLATION="required",
                                FLOWERLY_PTY_CGROUP_ROOT=str(root))
        self.process = None

    def start(self):
        with self.log.open("ab") as log:
            self.process = subprocess.Popen([str(self.binary)], env=self.environment,
                                            stdout=log, stderr=subprocess.STDOUT)
        for _ in range(300):
            if self.process.poll() is not None:
                raise AssertionError("flowerlyd died on startup: " + self.log.read_text())
            if self.socket.exists():
                try:
                    ok(self.socket, "session.list", {})
                    break
                except (OSError, AssertionError):
                    pass
            time.sleep(0.03)
        else:
            raise AssertionError("flowerlyd readiness timeout: " + self.log.read_text())
        assert "PTY delegation unavailable" not in self.log.read_text(), self.log.read_text()

    def kill(self):
        self.process.kill()
        self.process.wait(timeout=10)

    def close(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.kill()
        self.temporary.cleanup()

    def sessions(self):
        return {row["id"]: row for row in ok(self.socket, "session.list", {})["sessions"]}

    def pty_groups(self):
        return {entry.name for entry in self.root.iterdir()
                if entry.is_dir() and PTY_GROUP.fullmatch(entry.name)}

    def management_groups(self):
        return {entry.name for entry in self.root.iterdir()
                if entry.is_dir() and entry.name.startswith("management-")}


def start_descendant(daemon, spawned):
    """A setsid process that leaves the shell's session and process group."""
    token = ok(daemon.socket, "session.attach", {
        "id": spawned["id"], "control_token": spawned["control_token"]})["attachment_token"]
    ok(daemon.socket, "session.write", {
        "id": spawned["id"], "attachment_token": token,
        "data": "setsid sh -c 'echo FLOWERLY_DESC:$$; exec sleep 300' </dev/null &\n"})
    for _ in range(250):
        reply = ok(daemon.socket, "session.read", {
            "id": spawned["id"], "cursor": 0, "control_token": spawned["control_token"]})
        found = re.search(rb"FLOWERLY_DESC:(\d+)", base64.b64decode(reply["output_base64"]))
        if found:
            pid = int(found.group(1))
            original = identity(pid)
            assert original and original[0] not in "ZX", pid
            group = Path(f"/proc/{pid}/cgroup").read_text().strip().rsplit("/", 1)[-1]
            assert group == f"pty-{spawned['id']}", (group, spawned["id"])
            assert os.getsid(pid) != os.getsid(spawned["pid"]), "descendant kept the shell session"
            return pid, original
        time.sleep(0.02)
    raise AssertionError("setsid descendant did not start")


def tracked(spawned):
    processes = {}
    for pid in (spawned["pid"], spawned["supervisor_pid"]):
        processes[pid] = identity(pid)
    return processes


def assert_clean_restart(daemon, label):
    daemon.start()
    assert daemon.pty_groups() == set(), (label, daemon.pty_groups())
    assert len(daemon.management_groups()) == 1, (label, daemon.management_groups())
    running = [row for row in daemon.sessions().values() if row["state"].lower() == "running"]
    assert not running, (label, running)


def assert_unknown_and_refused(daemon, key, arguments, session_recorded):
    before = daemon.pty_groups()
    lookup = ok(daemon.socket, "session.spawn.lookup", {"operation_id": key})
    assert lookup["session_recorded"] is session_recorded, lookup
    assert lookup["guardian_supervised"] is False, lookup
    assert lookup["external_outcome"] == "unknown" and lookup["retry_safe"] is False, lookup
    retry = request(daemon.socket, "session.spawn", dict(arguments, operation_id=key))
    assert retry["type"] == "err" and retry.get("code") == "unavailable", retry
    changed = request(daemon.socket, "session.spawn",
                      dict(arguments, operation_id=key, shell="/bin/bash"))
    assert changed["type"] == "err" and changed.get("code") == "conflict", changed
    assert daemon.pty_groups() == before, "a refused retry created a PTY cgroup"
    return lookup


def case_daemon_sigkill(daemon):
    keyed = ok(daemon.socket, "session.spawn", {"shell": "/bin/sh", "operation_id": "matrix-live"})
    unkeyed = ok(daemon.socket, "session.spawn", {"shell": "/bin/sh"})
    processes = tracked(keyed) | tracked(unkeyed)
    for spawned in (keyed, unkeyed):
        pid, original = start_descendant(daemon, spawned)
        processes[pid] = original
    assert daemon.pty_groups() == {f"pty-{keyed['id']}", f"pty-{unkeyed['id']}"}
    daemon.kill()
    wait_gone(processes, "flowerlyd SIGKILL")
    assert daemon.pty_groups() == set(), daemon.pty_groups()
    assert_clean_restart(daemon, "flowerlyd SIGKILL")
    rows = daemon.sessions()
    for spawned in (keyed, unkeyed):
        row = rows[spawned["id"]]
        assert row["state"].lower() == "suspended", row
        assert row["presentation_state"].lower() == "detached", row
        stale = request(daemon.socket, "session.attach", {
            "id": spawned["id"], "control_token": spawned["control_token"]})
        assert stale["type"] == "err", stale
    lookup = assert_unknown_and_refused(daemon, "matrix-live", {"shell": "/bin/sh"}, True)
    assert lookup["session_id"] == keyed["id"], lookup
    fresh = ok(daemon.socket, "session.spawn", {"shell": "/bin/sh"})
    ok(daemon.socket, "session.terminate", {"id": fresh["id"], "control_token": fresh["control_token"]})
    print("PTY #74 flowerlyd SIGKILL: keyed and unkeyed shells, guardians and setsid "
          "descendants exited, no PTY cgroup left; restart reports suspended/detached, "
          "stale tokens refused, key outcome unknown, retry launches nothing, changed "
          "arguments conflict, fresh spawn works")


def crash_before_row(daemon, arguments):
    """SIGKILL flowerlyd after the guardian moved the shell into its cgroup
    but before the Session row commits; the insert waits on a real lock."""
    for attempt in range(8):
        known_rows = set(daemon.sessions())
        before = daemon.pty_groups()
        reply = {}
        caller = background_spawn(daemon.socket, arguments, reply)
        group = None
        deadline = time.monotonic() + 10
        while group is None and time.monotonic() < deadline:
            fresh = daemon.pty_groups() - before
            for name in fresh:
                procs = (daemon.root / name / "cgroup.procs").read_text().split()
                if procs:
                    group = name
            if group is None:
                time.sleep(0.001)
        assert group, "no PTY cgroup appeared for the spawn"
        lock = sqlite3.connect(daemon.database, timeout=30, isolation_level=None)
        lock.execute("BEGIN IMMEDIATE")
        guardians = {pid: identity(pid) for guardian in children(daemon.process.pid)
                     for pid in [guardian, *children(guardian)]}
        shell = int((daemon.root / group / "cgroup.procs").read_text().split()[0])
        processes = guardians | {shell: identity(shell)}
        daemon.kill()
        lock.execute("ROLLBACK")
        lock.close()
        caller.join(timeout=10)
        wait_gone(processes, "SIGKILL before the Session row")
        assert daemon.pty_groups() == before, daemon.pty_groups()
        assert_clean_restart(daemon, "SIGKILL before the Session row")
        session_id = group.removeprefix("pty-")
        if session_id in daemon.sessions() and session_id not in known_rows:
            continue  # the row won the race; try again
        assert reply.get("type") != "ok", reply
        return session_id
    raise AssertionError("could not hold the Session insert behind the write lock")


def case_crash_before_row(daemon):
    session_id = crash_before_row(daemon, {"shell": "/bin/sh", "operation_id": "matrix-window"})
    lookup = assert_unknown_and_refused(daemon, "matrix-window", {"shell": "/bin/sh"}, False)
    assert lookup["session_id"] == session_id, (lookup, session_id)
    crash_before_row(daemon, {"shell": "/bin/sh"})
    print("PTY #74 SIGKILL before the Session row (real SQLite write lock held): keyed "
          "and unkeyed guardian and shell exited, no PTY cgroup left; the key reports "
          "session_recorded=false, unknown outcome, retry refused")


def kill_supervision(daemon, spawned_pid=None):
    """SIGKILL the cleanup watcher first, then the guardian and flowerlyd, so
    only processes that left the shell's session survive in the cgroup."""
    guardians = children(daemon.process.pid)
    watchers = [child for guardian in guardians for child in children(guardian)
                if child != spawned_pid and not Path(f"/proc/{child}/cgroup").read_text()
                .strip().rsplit("/", 1)[-1].startswith("pty-")]
    assert watchers, "no PTY cleanup watcher found"
    for pid in watchers + guardians + [daemon.process.pid]:
        os.kill(pid, signal.SIGKILL)
    daemon.process.wait(timeout=10)


def case_total_supervision_loss(daemon):
    # (a) Session row present, (b) key reserved without a row, (c) no key, no row.
    for label, arguments, hold_row in (
            ("recorded", {"shell": "/bin/sh"}, False),
            ("reserved", {"shell": "/bin/sh", "operation_id": "matrix-orphan"}, True),
            ("unkeyed", {"shell": "/bin/sh"}, True)):
        lock = None
        if hold_row:
            before = daemon.pty_groups()
            reply = {}
            background_spawn(daemon.socket, arguments, reply)
            group = None
            for _ in range(5000):
                fresh = [name for name in daemon.pty_groups() - before
                         if (daemon.root / name / "cgroup.procs").read_text().split()]
                if fresh:
                    group = fresh[0]
                    break
                time.sleep(0.001)
            assert group, label
            lock = sqlite3.connect(daemon.database, timeout=30, isolation_level=None)
            lock.execute("BEGIN IMMEDIATE")
            shell = int((daemon.root / group / "cgroup.procs").read_text().split()[0])
            # Fork a setsid survivor from inside the cgroup without the PTY API:
            # the shell is not yet reachable before its Session row exists.
            # Not a process group leader, so setsid(1) does not fork.
            survivor = subprocess.Popen(
                ["sh", "-c", f"echo $$ > {daemon.root / group / 'cgroup.procs'} && exec setsid sleep 300"])
            time.sleep(0.2)
            assert survivor.poll() is None, ("survivor did not start", survivor.returncode)
            processes = {survivor.pid: identity(survivor.pid), shell: identity(shell)}
        else:
            spawned = ok(daemon.socket, "session.spawn", arguments)
            group = f"pty-{spawned['id']}"
            pid, original = start_descendant(daemon, spawned)
            processes = {pid: original}
            survivor = None
        kill_supervision(daemon)
        if lock:
            lock.execute("ROLLBACK")
            lock.close()
        time.sleep(0.5)
        living = [pid for pid, original in processes.items() if alive(pid, original)]
        assert living, f"{label}: nothing survived; the case did not exercise recovery"
        assert group in daemon.pty_groups(), (label, group)
        daemon.start()
        wait_gone(processes, f"restart after total supervision loss ({label})")
        if survivor:
            survivor.wait(timeout=5)
        assert daemon.pty_groups() == set(), (label, daemon.pty_groups())
        assert len(daemon.management_groups()) == 1, (label, daemon.management_groups())
    print("PTY #74 total supervision loss (watcher, guardian and flowerlyd SIGKILLed): "
          "surviving setsid descendants killed by restart recovery with a Session row, "
          "with only a reserved key, and with neither; stale management cgroups removed")


def case_concurrency(daemon):
    keys = [f"matrix-race-{index % 3}" for index in range(9)]
    with ThreadPoolExecutor(max_workers=9) as pool:
        replies = list(pool.map(lambda key: request(daemon.socket, "session.spawn",
                                                    {"shell": "/bin/sh", "operation_id": key},
                                                    timeout=30), keys))
    launched = [reply["data"] for reply in replies if reply["type"] == "ok"]
    refused = [reply for reply in replies if reply["type"] == "err"]
    assert len(launched) == 3, replies
    assert all(reply.get("code") == "unavailable" for reply in refused), refused
    assert len({spawned["id"] for spawned in launched}) == 3
    assert daemon.pty_groups() == {f"pty-{spawned['id']}" for spawned in launched}
    with ThreadPoolExecutor(max_workers=6) as pool:
        unkeyed = [reply["data"] for reply in pool.map(
            lambda _: request(daemon.socket, "session.spawn", {"shell": "/bin/sh"}, timeout=30),
            range(4)) if reply["type"] == "ok"]
        assert len(unkeyed) == 4
        processes = {}
        for spawned in launched + unkeyed:
            processes |= tracked(spawned)
        terminations = list(pool.map(lambda spawned: request(
            daemon.socket, "session.terminate",
            {"id": spawned["id"], "control_token": spawned["control_token"]}), launched))
        assert all(reply["type"] == "ok" for reply in terminations), terminations
    daemon.kill()
    wait_gone(processes, "concurrent spawn/terminate then SIGKILL")
    assert_clean_restart(daemon, "concurrency")
    for key in sorted(set(keys)):
        assert_unknown_and_refused(daemon, key, {"shell": "/bin/sh"}, True)
    print("PTY #74 concurrency: 9 spawns over 3 keys launch exactly 3 guardians, duplicates "
          "unavailable; concurrent terminate and unkeyed spawns, then SIGKILL and restart "
          "leave nothing running and no PTY cgroup")


def main(binary_directory):
    assert sys.platform == "linux" and os.geteuid() != 0
    root = Path(os.environ["FLOWERLY_PTY_CGROUP_ROOT"])
    assert os.environ.get("FLOWERLY_PTY_SHELL_UID"), "distinct shell UID required"
    daemon = Daemon(binary_directory, root)
    try:
        daemon.start()
        case_daemon_sigkill(daemon)
        case_crash_before_row(daemon)
        case_total_supervision_loss(daemon)
        case_concurrency(daemon)
    except BaseException:
        sys.stderr.write(daemon.log.read_text()[-6000:])
        raise
    finally:
        daemon.close()
    print("PTY_DAEMON_RECOVERY_MATRIX=PASS")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-pty-daemon-recovery.py BINARY_DIR")
    main(Path(sys.argv[1]).resolve())

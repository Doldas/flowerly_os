#!/usr/bin/env python3
"""Real-daemon IPC idempotency contract for optional operation_id.

Launches actual flowd, goald and sessiond processes on a shared file-backed
SQLite database. Confirms exact replay, conflict on mismatched args, durable
replay after daemon restart, and actor scoping via the trusted peer UID.
"""
import json
import os
import pathlib
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise AssertionError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def request(socket_path, name, args):
    correlation = str(uuid.uuid4())
    message = json.dumps({
        "version": 1,
        "id": correlation,
        "type": "command",
        "name": name,
        "args": args,
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        assert size < 8 * 1024 * 1024, size
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, (name, reply)
    assert reply["version"] == 1, (name, reply)
    return reply


def shutdown(socket_path):
    correlation = str(uuid.uuid4())
    message = json.dumps({
        "version": 1, "id": correlation, "type": "shutdown"
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation and reply["type"] == "ok", reply


def wait_ready(socket_path, process, log_path):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited: {log_path.read_text()}")
        if socket_path.exists():
            try:
                reply = request(socket_path, "flow.list" if socket_path.name == "flowd.sock" else
                                "goal.list" if socket_path.name == "goald.sock" else "session.list", {})
                if reply["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def main(bin_dir):
    with tempfile.TemporaryDirectory(prefix="flowerly-replay-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        processes = {}

        def start(name):
            log_path = root / f"{name}.log"
            with log_path.open("ab") as log:
                proc = subprocess.Popen(
                    [str(bin_dir / name)], env=env, stdout=log, stderr=subprocess.STDOUT
                )
            processes[name] = proc
            wait_ready(runtime / f"{name}.sock", proc, log_path)

        try:
            for name in ("flowd", "goald", "sessiond"):
                start(name)
            scenarios = [
                ("flowd", "flow.create",
                 {"name": "Persistent Flow", "operation_id": "flow-create-00001"}),
                ("goald", "goal.create",
                 {"objective": "Persistent Goal", "operation_id": "goal-create-00001"}),
                ("sessiond", "session.create",
                 {"kind": "local-pty", "operation_id": "session-create-00001"}),
            ]
            first = {}
            baseline = {
                service: request(runtime / f"{service}.sock",
                                 {"flowd": "flow.list", "goald": "goal.list",
                                  "sessiond": "session.list"}[service], {})["data"]["count"]
                for service in ("flowd", "goald", "sessiond")
            }
            for service, command, args in scenarios:
                sock = runtime / f"{service}.sock"
                result = request(sock, command, args)
                assert result["type"] == "ok", (service, result)
                assert result["data"]["id"], result
                repeated = request(sock, command, args)
                assert repeated["type"] == "ok", repeated
                assert repeated["data"] == result["data"], (service, result, repeated)
                first[service] = result["data"]["id"]

            # The same key with changed command payload must not be treated as
            # a harmless retry, even if it contains a different fresh request ID.
            conflict = request(
                runtime / "flowd.sock", "flow.create",
                {"name": "Different Flow", "operation_id": "flow-create-00001"},
            )
            assert conflict["type"] == "err", conflict
            assert conflict["code"] == "conflict", conflict

            bad = request(
                runtime / "goald.sock", "goal.create",
                {"objective": "Invalid", "operation_id": "??"},
            )
            assert bad["type"] == "err" and bad["code"] == "invalid_argument", bad

            # Goal-child creation is also non-idempotent and must use the same
            # operation record mechanism.
            child_args = {
                "id": first["goald"], "objective": "Child Goal",
                "operation_id": "goal-child-00001",
            }
            child = request(runtime / "goald.sock", "goal.child", child_args)
            assert child["type"] == "ok", child
            repeated_child = request(runtime / "goald.sock", "goal.child", child_args)
            assert repeated_child["type"] == "ok", repeated_child
            assert repeated_child["data"] == child["data"], (child, repeated_child)

            # Simulate a crash/reconnection window after a committed response:
            # stop the daemon, keep state.db, restart, and replay same key.
            shutdown(runtime / "flowd.sock")
            assert processes["flowd"].wait(timeout=5) == 0
            start("flowd")
            replay = request(
                runtime / "flowd.sock", "flow.create", scenarios[0][2]
            )
            assert replay["type"] == "ok", replay
            assert replay["data"]["id"] == first["flowd"], replay

            expected = {
                "flowd": baseline["flowd"] + 1,
                "goald": baseline["goald"] + 2,
                "sessiond": baseline["sessiond"] + 1,
            }
            for service, command in (
                ("flowd", "flow.list"), ("goald", "goal.list"),
                ("sessiond", "session.list"),
            ):
                listing = request(runtime / f"{service}.sock", command, {})
                assert listing["type"] == "ok", listing
                assert listing["data"]["count"] == expected[service], (
                    service, expected[service], listing
                )

            with sqlite3.connect(state / "state.db") as conn:
                rows = conn.execute(
                    "SELECT actor, command, operation_id FROM ipc_operations"
                ).fetchall()
            assert len(rows) == 4, rows
            assert all(actor == f"uid:{os.getuid()}" for actor, _, _ in rows), rows
            print(
                "IPC idempotency OK: creates/child, exact replay, mismatch, "
                "daemon restart, trusted actor and no duplicates"
            )
        finally:
            for process in processes.values():
                if process.poll() is None:
                    process.terminate()
            for process in processes.values():
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: python3 scripts/test-ipc-idempotency.py target/release")
    main(pathlib.Path(sys.argv[1]).resolve())

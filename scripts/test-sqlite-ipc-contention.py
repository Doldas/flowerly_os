#!/usr/bin/env python3
"""Black-box IPC liveness of a unified supervisor under external SQLite writer contention.

Runs built flowerlyd with isolated paths. The writer lock belongs to a
separate OS process. Assertions cover correlated replies, responsive Ping,
queued writes recovering after unlock, and bounded elapsed wall-clock time.
No network, external dependencies or privileged access required.
"""
import concurrent.futures
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

TIMEOUT = 10.0


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise RuntimeError("IPC socket closed before a complete response")
        data.extend(chunk)
    return bytes(data)


def request(path, kind, **fields):
    correlation = uuid.uuid4().hex
    payload = {"version": 1, "id": correlation, "type": kind, **fields}
    data = json.dumps(payload).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(TIMEOUT)
        conn.connect(str(path))
        conn.sendall(struct.pack("<I", len(data)) + data)
        size = struct.unpack("<I", recv_exact(conn, 4))[0]
        assert size < 8 * 1024 * 1024, size
        response = json.loads(recv_exact(conn, size))
    assert response["id"] == correlation, response
    assert response["version"] == 1, response
    return response


def wait_ready(path, process, log):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited early: {log.read_text()}")
        if path.exists():
            try:
                if request(path, "ping")["type"] == "ok":
                    return
            except (OSError, RuntimeError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"daemon did not become ready: {log.read_text()}")


def run_lock_holder(db):
    conn = sqlite3.connect(db, timeout=2, isolation_level=None)
    try:
        conn.execute("BEGIN IMMEDIATE")
        print("LOCKED", flush=True)
        sys.stdin.readline()
        conn.execute("ROLLBACK")
    finally:
        conn.close()


def main(bin_dir):
    with tempfile.TemporaryDirectory(prefix="flowerly-unified-contention-") as td:
        root = pathlib.Path(td)
        runtime, state = root / "runtime", root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        sock = runtime / "flowerlyd.sock"
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        daemon, holder = None, None
        pool = concurrent.futures.ThreadPoolExecutor(max_workers=2)
        log_path = root / "flowerlyd.log"

        def start():
            nonlocal daemon
            with log_path.open("ab") as log:
                daemon = subprocess.Popen([str(bin_dir / "flowerlyd")], env=env,
                                          stdout=log, stderr=subprocess.STDOUT)
            wait_ready(sock, daemon, log_path)

        def cmd(name, args):
            return request(sock, "command", name=name, args=args)

        def lock_writer():
            nonlocal holder
            assert holder is None
            holder = subprocess.Popen(
                [sys.executable, __file__, "--hold-lock", str(state / "state.db")],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, text=True)
            assert holder.stdout.readline().strip() == "LOCKED", "SQLite lock failed"

        def release_writer():
            nonlocal holder
            holder.stdin.write("\n")
            holder.stdin.flush()
            holder.communicate(timeout=5)
            assert holder.returncode == 0
            holder = None

        def healthy(busy=False):
            t = time.monotonic()
            ping = request(sock, "ping")
            status = request(sock, "status")
            assert ping["type"] == "ok" and status["type"] == "ok", (ping, status)
            assert time.monotonic() - t < 3, "control plane blocked by SQLite writer"
            assert status["data"]["service"] == "flowerlyd", status
            if busy:
                assert status["data"]["details"]["ready"] is False, status

        def blocked(name, args):
            lock_writer()
            future = pool.submit(cmd, name, args)
            try:
                deadline = time.monotonic() + 2
                while time.monotonic() < deadline:
                    reply = request(sock, "status")
                    if reply["data"]["details"]["ready"] is False:
                        break
                    time.sleep(0.04)
                else:
                    raise AssertionError("the Store never observed the external writer")
                healthy(busy=True)
                answer = future.result(timeout=TIMEOUT + 2)
                assert answer["type"] == "err" and answer.get("code") == "unavailable", (name, answer)
            finally:
                release_writer()

        try:
            start()
            healthy()
            lock_writer()
            future = pool.submit(cmd, "flow.create", {"name": "Short external lock"})
            time.sleep(0.25)
            healthy(busy=True)
            release_writer()
            flow_reply = future.result(timeout=TIMEOUT)
            assert flow_reply["type"] == "ok", flow_reply
            flow_id = flow_reply["data"]["id"]
            goal_reply = cmd("goal.create", {"objective": "External writer goal"})
            assert goal_reply["type"] == "ok", goal_reply
            goal_id = goal_reply["data"]["id"]
            session_reply = cmd("session.create", {})
            assert session_reply["type"] == "ok", session_reply
            session_id = session_reply["data"]["id"]

            # Shared in-process Store serializes writes, so probe one domain
            # per external lock rather than assuming independent daemon mutexes.
            for name, args in [
                ("flow.create", {"name": "Long locked Flow"}),
                ("goal.create", {"objective": "Long locked Goal"}),
                ("session.create", {"kind": "test-metadata"}),
                ("flow.activate", {"id": flow_id}),
                ("goal.complete", {"id": goal_id}),
                ("session.terminate", {"id": session_id}),
                ("execution.create", {}),
                ("generation.create", {}),
                ("provenance.add", {"from": goal_id, "relation": "belongs_to", "to": flow_id}),
            ]:
                blocked(name, args)
                recovered = cmd(name, args)
                assert recovered["type"] == "ok", (name, recovered)

            originals = {}
            for name, args in [
                ("flow.create", {"name": "Replay Flow"}),
                ("goal.create", {"objective": "Replay Goal"}),
                ("session.create", {"kind": "test-metadata"}),
            ]:
                keyed = dict(args, operation_id="busy-" + name.replace(".", "-"))
                first, second = cmd(name, keyed), cmd(name, keyed)
                assert first["type"] == "ok" and second["data"] == first["data"], (name, first, second)
                originals[name] = (keyed, first["data"])
                conflict = cmd(name, dict(keyed, unexpected="change"))
                assert conflict["type"] == "err" and conflict.get("code") == "conflict", conflict

            parent = cmd("goal.create", {"objective": "Parent goal for replay"})
            assert parent["type"] == "ok", parent
            child = {"id": parent["data"]["id"], "objective": "Only child",
                     "operation_id": "busy-child-replay"}
            first, repeated = cmd("goal.child", child), cmd("goal.child", child)
            assert first["type"] == "ok" and repeated["data"] == first["data"], (first, repeated)

            assert request(sock, "shutdown")["type"] == "ok"
            assert daemon.wait(timeout=6) == 0
            start()
            for name, (args, expected) in originals.items():
                reply = cmd(name, args)
                assert reply["type"] == "ok" and reply["data"] == expected, (name, reply)
            print("unified SQLite contention: external busy, prompt status, recovery and durable replay OK")
        finally:
            if holder is not None and holder.poll() is None:
                holder.terminate()
                try:
                    holder.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    holder.kill()
                    holder.communicate(timeout=3)
            if daemon is not None and daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=5)
            pool.shutdown(wait=True, cancel_futures=True)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--hold-lock":
        run_lock_holder(sys.argv[2])
    elif len(sys.argv) == 2:
        main(pathlib.Path(sys.argv[1]).resolve())
    else:
        raise SystemExit("usage: python3 scripts/test-sqlite-ipc-contention.py target/release")

#!/usr/bin/env python3
"""Full Linux process black box: Flow A -> B -> A and real PTY reattach."""
import base64
import json
import os
from pathlib import Path
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import uuid

def recv_exact(conn, size):
    data = bytearray()
    while len(data) < size:
        part = conn.recv(size - len(data))
        if not part:
            raise AssertionError("IPC response truncated")
        data.extend(part)
    return bytes(data)

def call(path, name, args):
    request_id = str(uuid.uuid4())
    payload = json.dumps({
        "version": 1, "id": request_id, "type": "command",
        "name": name, "args": args
    }).encode()
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(12)
        sock.connect(str(path))
        sock.sendall(struct.pack("<I", len(payload)) + payload)
        size = struct.unpack("<I", recv_exact(sock, 4))[0]
        result = json.loads(recv_exact(sock, size))
    assert result["id"] == request_id and result["version"] == 1, result
    return result

def ok(path, name, args):
    reply = call(path, name, args)
    assert reply["type"] == "ok", (name, reply)
    return reply["data"]

def start(bin_dir, name, env, processes, root):
    path = Path(env["AGENTIC_RUNTIME_DIR"]) / (name + ".sock")
    log = root / (name + ".log")
    with log.open("wb") as output:
        child = subprocess.Popen([str(bin_dir / name)], env=env, stdout=output, stderr=subprocess.STDOUT)
    processes.append(child)
    for _ in range(200):
        if child.poll() is not None:
            raise AssertionError("daemon died: " + log.read_text())
        if path.exists():
            try:
                if call(path, "session.list" if name == "sessiond" else "flow.list", {})["type"] == "ok":
                    return path
            except (OSError, AssertionError):
                pass
        time.sleep(.03)
    raise AssertionError("daemon failed to start: " + log.read_text())

def main(bin_dir):
    assert sys.platform == "linux"
    with tempfile.TemporaryDirectory(prefix="flowerly-flow-pty-") as temp:
        root = Path(temp)
        runtime, state = root / "runtime", root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        children = []
        try:
            flow = start(bin_dir, "flowd", env, children, root)
            session = start(bin_dir, "sessiond", env, children, root)
            a = ok(flow, "flow.create", {"name": "PTY Flow A"})["id"]
            b = ok(flow, "flow.create", {"name": "PTY Flow B"})["id"]
            ok(flow, "flow.activate", {"id": a})
            first = ok(session, "session.spawn", {"flow_id": a, "shell": "/bin/sh"})
            sid, pid = first["id"], first["pid"]
            assert pid > 0, first
            attached = ok(session, "session.attach", {"id": sid, "cursor": 0})
            assert attached["pid"] == pid and attached["running"] is True
            ok(session, "session.resize", {"id": sid, "rows": 42, "cols": 92})
            marker = "FLOWERLY_A_B_A_STILL_ALIVE"
            text = "echo " + marker + "\n"
            written = ok(session, "session.write", {"id": sid, "data": text})
            assert written["written"] == len(text), written
            ok(session, "session.detach", {"id": sid})
            ok(flow, "flow.activate", {"id": b})
            assert ok(flow, "flow.list", {})["count"] >= 3
            ok(flow, "flow.activate", {"id": a})
            for _ in range(100):
                replay = ok(session, "session.attach", {"id": sid, "cursor": 0})
                if marker.encode() in base64.b64decode(replay["output_base64"]):
                    break
                time.sleep(.03)
            else:
                raise AssertionError("PTY output lost across Flow switch")
            assert replay["pid"] == pid and replay["running"] is True
            # Hold an independent SQLite writer lock. Live PTY control
            # cannot await a Store mutex just to authorize the same actor.
            with sqlite3.connect(state / "state.db", timeout=2, isolation_level=None) as db:
                db.execute("BEGIN IMMEDIATE")
                began = time.monotonic()
                alive = ok(session, "session.status", {"id": sid})
                assert time.monotonic() - began < 2, "PTY status blocked on SQLite"
                assert alive["running"] is True, alive
                db.execute("ROLLBACK")
            invalid_shell = call(session, "session.spawn", {"shell": "/bin/false"})
            assert invalid_shell["type"] == "err" and invalid_shell["code"] == "invalid_argument"
            listing = ok(session, "session.list", {})["sessions"]
            assert any(row["id"] == sid and row["pid"] == pid and row["flow_id"] == a for row in listing)
            stopped = ok(session, "session.terminate", {"id": sid})
            assert stopped["terminated"] is True, stopped
            # Observe natural exit and reconcile the recorded Session state.
            second_shell = ok(session, "session.spawn", {"flow_id": a})
            another_id = second_shell["id"]
            ok(session, "session.write", {"id": another_id, "data": "exit\n"})
            for _ in range(100):
                state_reply = ok(session, "session.status", {"id": another_id})
                if state_reply["running"] is False:
                    break
                time.sleep(.03)
            else:
                raise AssertionError("natural PTY exit not observed")
            listed = ok(session, "session.list", {})["sessions"]
            assert any(row["id"] == another_id and
                       row["state"].lower() == "exited" for row in listed), listed
            print("Linux Flow A -> B -> A PTY: same Session/PID, replay, SQLite liveness, natural exit, termination")
        finally:
            for proc in children:
                if proc.poll() is None:
                    proc.terminate()
            for proc in children:
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)

if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: test-sessiond-pty-flow.py target/release")
    main(Path(sys.argv[1]).resolve())

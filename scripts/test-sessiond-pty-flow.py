#!/usr/bin/env python3
"""Full Linux process black box: Flow A -> B -> A and real PTY reattach."""
import base64
from concurrent.futures import ThreadPoolExecutor
import json
import os
import re
import signal
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
            control = first["control_token"]
            assert control
            assert pid > 0, first
            denied_attach = call(session, "session.attach", {"id": sid})
            assert denied_attach["type"] == "err", denied_attach
            denied_read = call(session, "session.read", {"id": sid, "cursor": 0})
            assert denied_read["type"] == "err", denied_read
            denied_terminate = call(session, "session.terminate", {"id": sid})
            assert denied_terminate["type"] == "err", denied_terminate
            read_only_status = ok(session, "session.status", {"id": sid})
            assert read_only_status["running"] is True
            assert "output_base64" not in read_only_status, read_only_status
            bad_control = call(session, "session.attach", {"id": sid, "control_token": "wrong"})
            assert bad_control["type"] == "err", bad_control
            attached = ok(session, "session.attach", {"id": sid, "cursor": 0, "control_token": control})
            assert attached["pid"] == pid and attached["running"] is True
            first_token = attached["attachment_token"]
            assert first_token
            double_attach = call(session, "session.attach", {"id": sid, "control_token": control})
            assert double_attach["type"] == "err" and double_attach["code"] == "conflict", double_attach
            without_token = call(session, "session.write", {"id": sid, "data": "ignored"})
            assert without_token["type"] == "err", without_token
            ok(session, "session.resize", {"id": sid, "attachment_token": first_token, "rows": 42, "cols": 92})
            marker = "FLOWERLY_A_B_A_STILL_ALIVE"
            text = "echo " + marker + "\n"
            written = ok(session, "session.write", {"id": sid, "attachment_token": first_token, "data": text})
            assert written["written"] == len(text), written
            # A separate, long-running Linux process must survive the switch.
            job_command = "sleep 60 & echo FLOWERLY_JOB_PID:$!\n"
            ok(session, "session.write", {"id": sid, "attachment_token": first_token, "data": job_command})
            for _ in range(100):
                observed = ok(session, "session.read", {"id": sid, "cursor": 0, "control_token": control})
                match = re.search(rb"FLOWERLY_JOB_PID:(\d+)",
                                  base64.b64decode(observed["output_base64"]))
                if match:
                    break
                time.sleep(.03)
            else:
                raise AssertionError("background process PID not observed")
            job_pid = int(match.group(1))
            ok(session, "session.detach", {"id": sid, "attachment_token": first_token})
            stale_writer = call(session, "session.write", {"id": sid, "attachment_token": first_token, "data": "ignored"})
            assert stale_writer["type"] == "err", stale_writer
            ok(flow, "flow.activate", {"id": b})
            assert ok(flow, "flow.list", {})["count"] >= 3
            ok(flow, "flow.activate", {"id": a})
            for _ in range(100):
                replay = ok(session, "session.attach", {"id": sid, "cursor": 0, "control_token": control})
                if marker.encode() in base64.b64decode(replay["output_base64"]):
                    break
                time.sleep(.03)
            else:
                raise AssertionError("PTY output lost across Flow switch")
            assert replay["pid"] == pid and replay["running"] is True
            second_token = replay["attachment_token"]
            assert second_token and second_token != first_token
            os.kill(job_pid, 0)
            with open("/proc/" + str(job_pid) + "/stat", encoding="utf-8") as stat:
                assert stat.read().split(") ", 1)[1][0] != "Z", "job is zombie"
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
            # Job control can place background children in separate groups.
            # Explicitly stop the job before tearing down its shell.
            ok(session, "session.write", {"id": sid, "attachment_token": second_token, "data": f"kill {job_pid}\n"})
            stopped = ok(session, "session.terminate", {"id": sid, "control_token": control})
            assert stopped["terminated"] is True, stopped
            # Observe natural exit and reconcile the recorded Session state.
            second_shell = ok(session, "session.spawn", {"flow_id": a})
            another_id = second_shell["id"]
            another_control = second_shell["control_token"]
            natural_attachment = ok(session, "session.attach", {"id": another_id, "control_token": another_control})
            ok(session, "session.write", {"id": another_id, "attachment_token": natural_attachment["attachment_token"], "data": "exit\n"})
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
            # Competing client attach calls must not both obtain writer authority.
            raced = ok(session, "session.spawn", {"flow_id": a})
            race_id = raced["id"]
            race_control = raced["control_token"]
            with ThreadPoolExecutor(max_workers=2) as pool:
                responses = list(pool.map(
                    lambda _: call(session, "session.attach", {"id": race_id, "control_token": race_control}),
                    range(2),
                ))
            winners = [r for r in responses if r["type"] == "ok"]
            losers = [r for r in responses if r["type"] == "err"]
            assert len(winners) == 1 and len(losers) == 1, responses
            assert losers[0]["code"] == "conflict", losers
            assert ok(session, "session.read", {"id": race_id, "cursor": 0, "control_token": race_control})["running"]
            won_token = winners[0]["data"]["attachment_token"]
            ok(session, "session.detach", {"id": race_id, "attachment_token": won_token})
            ok(session, "session.terminate", {"id": race_id, "control_token": race_control})
            # A live daemon must detect the unexpectedly SIGKILLed guardian,
            # persist lost supervision, and never claim the old shell is live.
            crashed = ok(session, "session.spawn", {"flow_id": a})
            crashed_id = crashed["id"]
            assert crashed["supervisor_pid"] > 0, crashed
            os.kill(crashed["supervisor_pid"], signal.SIGKILL)
            for _ in range(100):
                reply = call(session, "session.status", {"id": crashed_id})
                if reply["type"] == "err" and reply["code"] == "unavailable":
                    state_row = next(row for row in ok(session, "session.list", {})["sessions"]
                                     if row["id"] == crashed_id)
                    if state_row["state"].lower() == "suspended":
                        assert state_row["presentation_state"].lower() == "detached"
                        break
                time.sleep(.03)
            else:
                raise AssertionError("guardian SIGKILL not reconciled to unavailable/detached")
            # The process supervisor is ephemeral: on restart the persisted
            # ID must NOT claim that its old shell is still attached/running.
            stale = ok(session, "session.spawn", {"flow_id": a})
            stale_id = stale["id"]
            stale_control = stale["control_token"]
            session_process = children[-1]
            session_process.terminate()
            session_process.wait(timeout=5)
            # The guardian parent-death path closes its own child; no stale
            # PID may be used to attach or send signals on daemon restart.
            session = start(bin_dir, "sessiond", env, children, root)
            after_restart = ok(session, "session.list", {})["sessions"]
            record = next(row for row in after_restart if row["id"] == stale_id)
            assert record["state"].lower() == "suspended", record
            assert record["presentation_state"].lower() == "detached", record
            reattach = call(session, "session.attach", {"id": stale_id, "control_token": stale_control})
            assert reattach["type"] == "err", reattach
            assert reattach["code"] == "unavailable", reattach
            print("PTY daemon restart: stale Session remains identified but unavailable/detached")
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

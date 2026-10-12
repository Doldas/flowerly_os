#!/usr/bin/env python3
"""Black-box IPC liveness against two real daemons and an external SQLite writer.

Runs built flowd/goald with isolated paths. The writer lock belongs to a
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
    with tempfile.TemporaryDirectory(prefix="flowerly-ipc-contention-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        services = []
        holder = None
        executor = concurrent.futures.ThreadPoolExecutor(max_workers=2)
        try:
            for name in ("flowd", "goald"):
                log_path = root / f"{name}.log"
                log = log_path.open("wb")
                proc = subprocess.Popen([str(bin_dir / name)], env=env, stdout=log, stderr=subprocess.STDOUT)
                log.close()
                services.append((name, proc, log_path))
                wait_ready(runtime / f"{name}.sock", proc, log_path)

            db = state / "state.db"
            holder = subprocess.Popen(
                [sys.executable, __file__, "--hold-lock", str(db)],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            assert holder.stdout.readline().strip() == "LOCKED", "SQLite writer lock did not acquire"
            # Separate daemon processes now contend for the SAME SQLite writer.
            flow = executor.submit(
                request, runtime / "flowd.sock", "command",
                name="flow.create", args={"name": "External lock Flow"},
            )
            goal = executor.submit(
                request, runtime / "goald.sock", "command",
                name="goal.create", args={"objective": "External lock Goal"},
            )
            time.sleep(0.25)
            for service in ("flowd", "goald"):
                before = time.monotonic()
                answer = request(runtime / f"{service}.sock", "ping")
                duration = time.monotonic() - before
                assert answer["type"] == "ok", (service, answer)
                assert duration < 2, f"{service} Ping blocked for {duration:.3f}s"

            holder.stdin.write("\n")
            holder.stdin.flush()
            holder.communicate(timeout=5)
            assert holder.returncode == 0, f"lock holder failed: {holder.returncode}"

            for name, pending in (("flowd", flow), ("goald", goal)):
                response = pending.result(timeout=TIMEOUT)
                assert response["type"] == "ok", (name, response)
            print("IPC SQLite contention OK: external writer, two services, responsive Ping, recovered writes")
        finally:
            if holder is not None and holder.poll() is None:
                holder.terminate()
                try:
                    holder.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    holder.kill()
                    holder.communicate(timeout=3)
            for _, proc, _ in services:
                if proc.poll() is None:
                    proc.terminate()
            for name, proc, log in services:
                try:
                    proc.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=4)
                if proc.returncode not in (0, -15):
                    print(f"{name} exit {proc.returncode}: {log.read_text()}", file=sys.stderr)
            executor.shutdown(wait=True, cancel_futures=True)


if __name__ == "__main__":
    if len(sys.argv) == 3 and sys.argv[1] == "--hold-lock":
        run_lock_holder(sys.argv[2])
    elif len(sys.argv) == 2:
        main(pathlib.Path(sys.argv[1]).resolve())
    else:
        raise SystemExit("usage: python3 scripts/test-sqlite-ipc-contention.py target/release")

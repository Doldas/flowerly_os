#!/usr/bin/env python3
"""Black-box two-daemon boundary, durable state, and IPC regression."""
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def read_exact(connection, size):
    data = bytearray()
    while len(data) < size:
        part = connection.recv(size - len(data))
        if not part:
            raise AssertionError("truncated IPC frame")
        data.extend(part)
    return bytes(data)


def rpc(path, kind, name=None, args=None):
    request_id = str(uuid.uuid4())
    payload = {"version": 1, "id": request_id, "type": kind}
    if kind == "command":
        payload.update(name=name, args=args or {})
    body = json.dumps(payload).encode()
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(9)
        conn.connect(str(path))
        conn.sendall(struct.pack("<I", len(body)) + body)
        length = struct.unpack("<I", read_exact(conn, 4))[0]
        assert length < 1000000
        reply = json.loads(read_exact(conn, length))
    assert reply["id"] == request_id, reply
    return reply


def good(path, name, args):
    result = rpc(path, "command", name, args)
    assert result["type"] == "ok", result
    return result["data"]


def begin(binary, runtime, state):
    runtime.mkdir(parents=True, mode=0o700, exist_ok=True)
    state.mkdir(parents=True, mode=0o700, exist_ok=True)
    environment = dict(os.environ,
                       AGENTIC_RUNTIME_DIR=str(runtime),
                       AGENTIC_STATE_DIR=str(state))
    proc = subprocess.Popen([str(binary)], env=environment,
                            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    sock = runtime / (binary.name + ".sock")
    for _ in range(150):
        if proc.poll() is not None:
            error = proc.stderr.read().decode(errors="replace")
            raise AssertionError(f"{binary.name} died: {error}")
        if sock.exists():
            try:
                if rpc(sock, "ping")["type"] == "ok":
                    return proc, sock
            except (OSError, AssertionError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"{binary.name} not ready")


def shutdown(proc, sock):
    """Require a clean IPC shutdown; force cleanup only after recording failure."""
    failure = None
    try:
        assert proc.poll() is None, f"{sock.name} exited before shutdown"
        reply = rpc(sock, "shutdown")
        assert reply["type"] == "ok", (sock, reply)
        try:
            code = proc.wait(timeout=6)
        except subprocess.TimeoutExpired as error:
            raise AssertionError(f"{sock.name} did not drain within 6 seconds") from error
        assert code == 0, f"{sock.name} shutdown exit code {code}"
        assert not sock.exists(), f"{sock.name} socket remained after shutdown"
    except (OSError, AssertionError) as error:
        failure = error
    finally:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=6)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=6)
        if proc.stderr:
            proc.stderr.close()
    if failure is not None:
        raise AssertionError(f"graceful IPC shutdown failed: {failure}") from failure


def main(bin_dir):
    with tempfile.TemporaryDirectory(prefix="flowerly-unified-") as temp:
        root = Path(temp)
        flowerlyd = Path(bin_dir) / "flowerlyd"
        evolutiond = Path(bin_dir) / "evolutiond"
        supervisor, sock = begin(flowerlyd, root / "runtime", root / "state")
        hypervisor, evosock = begin(evolutiond, root / "evolution-runtime", root / "evolution-state")
        try:
            for service, path in (("flowerlyd", sock), ("evolutiond", evosock)):
                ping = rpc(path, "ping")
                assert ping["type"] == "ok", (service, ping)
                status_reply = rpc(path, "status")
                assert status_reply["type"] == "ok", (service, status_reply)
                assert status_reply["data"]["service"] == service, status_reply
            status = rpc(sock, "status")
            assert status["data"]["service"] == "flowerlyd", status
            boundary = rpc(evosock, "status")
            assert boundary["data"]["details"]["provisioned"] is False, boundary
            experiment = good(evosock, "evolution.experiment-status", {})
            assert experiment["status"] == "unavailable" and experiment["provisioned"] is False
            denied = rpc(evosock, "command", name="evolution.run_experiment", args={})
            assert denied["type"] == "err" and denied["code"] == "unavailable", denied
            assert {p.name for p in (root / "runtime").glob("*.sock")} == {"flowerlyd.sock"}
            assert {p.name for p in (root / "evolution-runtime").glob("*.sock")} == {"evolutiond.sock"}
            created = good(sock, "flow.create", {"name": "Unified test", "operation_id": "unified-flow-1"})
            repeat = good(sock, "flow.create", {"name": "Unified test", "operation_id": "unified-flow-1"})
            assert repeat["id"] == created["id"], (created, repeat)
            goal = good(sock, "goal.create", {"objective": "Lifecycle test"})
            assert goal["id"].startswith("goal-")
            session = good(sock, "session.create", {})
            assert session["id"].startswith("session-")
            listed = good(sock, "flow.list", {})
            assert any(item["id"] == created["id"] for item in listed["flows"])
            good(sock, "capability.list", {})
            assert (root / "state" / "state.db").is_file()
            print("Unified flowerlyd modules, bounded IPC and persistence: OK")
        finally:
            errors = []
            for process, path in ((supervisor, sock), (hypervisor, evosock)):
                try:
                    shutdown(process, path)
                except AssertionError as error:
                    errors.append(str(error))
            assert not errors, "; ".join(errors)
        resumed, rsock = begin(flowerlyd, root / "runtime", root / "state")
        try:
            flow = good(rsock, "flow.list", {})
            assert any(x["name"] == "Unified test" for x in flow["flows"])
            print("Single-daemon restart preserves durable state: OK")
        finally:
            shutdown(resumed, rsock)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-unified-runtime.py target/release")
    main(Path(sys.argv[1]))

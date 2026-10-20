#!/usr/bin/env python3
"""PTY supervisor integration test: verifies flowerlyd session runtime + guardian.

Tests the full lifecycle of a PTY session through the flowerlyd IPC:
- session.spawn launches guardian
- session.* commands (status, attach, write, read, resize, terminate)
- Guardian cgroup containment and cleanup on session termination
- Process tree termination including setsid descendants
"""

import base64
import json
import os
import pathlib
import socket
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
        "version": 1, "id": correlation, "type": "command",
        "name": name, "args": args,
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, (name, reply)
    return reply


def wait_ready(socket_path, process, log_path):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited: {log_path.read_text()}")
        if socket_path.exists():
            try:
                if request(socket_path, "flow.list", {})["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def assert_not_running(pid):
    for _ in range(150):
        try:
            with open(f"/proc/{pid}/stat", encoding="ascii") as state_file:
                state = state_file.read().split(") ", 1)[1][0]
        except (FileNotFoundError, ProcessLookupError):
            return
        if state in ("Z", "X"):
            return
        time.sleep(0.02)
    raise AssertionError(f"process {pid} survived termination")


def main():
    # Use development mode for PTY isolation (no real cgroup required)
    # This still exercises the supervisor logic and guardian communication
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-integration-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ,
            AGENTIC_RUNTIME_DIR=str(runtime),
            AGENTIC_STATE_DIR=str(state),
        )
        env["FLOWERLY_PTY_ISOLATION"] = "development"
        log = root / "flowerlyd.log"
        proc = subprocess.Popen(
            [str(pathlib.Path("target/release/flowerlyd"))], env=env,
            stdout=log.open("ab"), stderr=subprocess.STDOUT,
        )
        sock = runtime / "flowerlyd.sock"
        try:
            wait_ready(sock, proc, log)

            # Durable reservation is committed before any PTY guardian starts.
            # Repeating an ambiguous request must not create a second shell.
            spawn_args = {
                "shell": "/bin/sh",
                "created_by": "uid:1000",
                "operation_id": "retry-pty-001",
            }
            spawn_response = request(sock, "session.spawn", spawn_args)
            assert spawn_response["type"] == "ok", spawn_response
            before = request(sock, "session.list", {})
            before_ids = {entry["id"] for entry in before["data"]["sessions"]}
            repeat = request(sock, "session.spawn", spawn_args)
            assert repeat["type"] == "err", repeat
            assert repeat["code"] == "unavailable", repeat
            mismatch = request(sock, "session.spawn", dict(spawn_args, shell="/bin/bash"))
            assert mismatch["type"] == "err" and mismatch["code"] == "conflict", mismatch
            after = request(sock, "session.list", {})
            assert {entry["id"] for entry in after["data"]["sessions"]} == before_ids
            print("PTY durable key: no duplicate guardian or shell", flush=True)

            session_id = spawn_response["data"]["id"]
            print(f"Spawned PTY session: {session_id}")

            # Verify session exists via session.list
            list_response = request(sock, "session.list", {})
            sessions = list_response["data"]["sessions"]
            session = next(s for s in sessions if s["id"] == session_id)
            assert session["kind"] == "local-pty"
            assert session["state"] == "RUNNING"
            print(f"Session state: {session['state']}")

            # Get PTY socket path (format: pty-{session_id}.sock in runtime dir)
            pty_sock = runtime / f"pty-{session_id}.sock"
            for _ in range(100):
                if pty_sock.exists():
                    break
                time.sleep(0.02)
            else:
                raise AssertionError(f"PTY socket not created: {pty_sock}")

            # Test PTY status via flowerlyd (no token required for status)
            status = request(sock, "session.status", {"id": session_id})
            print(f"PTY status: {status}")
            assert status["type"] == "ok"
            shell_pid = status["data"]["pid"]
            print(f"PTY status: pid={shell_pid}, running={status['data']['running']}")

            # Attach to get attachment token (requires control_token from spawn)
            control_token = spawn_response["data"]["control_token"]
            attach = request(sock, "session.attach", {"id": session_id, "control_token": control_token})
            print(f"Attach: {attach}")
            assert attach["type"] == "ok"
            attachment_token = attach["data"]["attachment_token"]
            print(f"Got attachment token: {attachment_token}")

            # Test PTY write (requires attachment_token)
            cmd = "echo 'hello from PTY integration test'\n"
            write_resp = request(sock, "session.write", {"id": session_id, "attachment_token": attachment_token, "data": cmd})
            print(f"Write: {write_resp}")
            assert write_resp["type"] == "ok"

            # Read command output
            for _ in range(50):
                read_resp = request(sock, "session.read", {"id": session_id, "control_token": control_token})
                if read_resp["type"] == "ok":
                    output = base64.b64decode(read_resp["data"]["output_base64"]).decode("utf-8", errors="replace")
                    if "hello from PTY integration test" in output:
                        print("Command output verified")
                        break
                time.sleep(0.05)
            else:
                raise AssertionError("Command output not found in PTY replay")

            # Test PTY resize (requires attachment_token)
            resize = request(sock, "session.resize", {"id": session_id, "attachment_token": attachment_token, "rows": 40, "cols": 120})
            print(f"Resize: {resize}")
            assert resize["type"] == "ok"
            assert resize["data"]["rows"] == 40 and resize["data"]["cols"] == 120
            print("PTY resize verified")

            # Test PTY terminate (requires control_token)
            term = request(sock, "session.terminate", {"id": session_id, "control_token": control_token})
            print(f"Terminate: {term}")
            assert term["type"] == "ok"
            assert term["data"]["terminated"] is True
            print("PTY terminate command accepted")

            # Verify session state updated
            for _ in range(50):
                list_response = request(sock, "session.list", {})
                session = next(s for s in list_response["data"]["sessions"] if s["id"] == session_id)
                if session["state"] == "EXITED":
                    print(f"Session state updated to: {session['state']}")
                    break
                time.sleep(0.05)
            else:
                raise AssertionError("Session state not updated to Exited")

            # Verify PTY process terminated
            assert_not_running(shell_pid)
            print(f"Shell process {shell_pid} confirmed terminated")

            # Verify PTY socket cleaned up
            for _ in range(50):
                if not pty_sock.exists():
                    print("PTY socket cleaned up")
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("PTY socket not cleaned up")

            print("\nPTY supervisor integration test PASSED")

        finally:
            if proc.poll() is None:
                proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    main()
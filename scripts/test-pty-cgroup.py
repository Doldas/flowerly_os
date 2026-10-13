#!/usr/bin/env python3
import base64
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time

GUARDIAN = Path(__file__).with_name("pty-guardian.py")


def request(path, command):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(4)
        client.connect(str(path))
        client.sendall(json.dumps(command).encode() + b"\n")
        response = json.loads(client.makefile("rb").readline())
        assert "error" not in response, response
        return response


def assert_not_running(pid):
    for _ in range(150):
        try:
            with open(f"/proc/{pid}/stat", encoding="ascii") as state_file:
                state = state_file.read().split(") ", 1)[1][0]
        except FileNotFoundError:
            return
        if state in ("Z", "X"):
            return
        time.sleep(0.02)
    raise AssertionError(f"process {pid} survived cgroup termination")


def require_explicit_delegation(root):
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-strict-") as folder:
        path = Path(folder) / "strict.sock"
        env = dict(os.environ, FLOWERLY_PTY_ISOLATION="required")
        env.pop("FLOWERLY_PTY_CGROUP_ROOT", None)
        result = subprocess.run(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, capture_output=True, timeout=5,
        )
        assert result.returncode != 0, "guardian silently bypassed required cgroup"
        assert not path.exists(), "guardian exposed control socket without a cgroup"
    if root is None:
        print("Production fail-closed verified; delegated cgroup e2e NOT VERIFIED")


def exercise_real_cgroup(root):
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-cgroup-") as folder:
        path = Path(folder) / "cgroup.sock"
        env = dict(os.environ, FLOWERLY_PTY_ISOLATION="required",
                   FLOWERLY_PTY_CGROUP_ROOT=root)
        guardian = subprocess.Popen(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        descendant_pid = None
        try:
            for _ in range(150):
                if guardian.poll() is not None:
                    raise AssertionError(guardian.stderr.read().decode())
                if path.exists():
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("cgroup guardian startup timeout")
            identity = request(path, {"op": "status"})
            shell_pid = identity["pid"]
            command = (
                b"python3 -c 'import subprocess; p=subprocess.Popen("
                b"[\"sleep\",\"60\"],start_new_session=True);"
                b"print(\"FLOWERLY_SETSID:%s\"%p.pid,flush=True)'\n"
            )
            assert request(path, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
            for _ in range(150):
                reply = request(path, {"op": "read", "cursor": 0})
                match = re.search(
                    rb"FLOWERLY_SETSID:(\d+)",
                    base64.b64decode(reply["output_base64"]),
                )
                if match:
                    descendant_pid = int(match.group(1))
                    break
                time.sleep(0.02)
            assert descendant_pid, "escaped-session process PID not reported"
            assert os.getsid(descendant_pid) != shell_pid, "child did not escape terminal session"
            guardian.kill()
            guardian.wait(timeout=5)
            assert_not_running(shell_pid)
            assert_not_running(descendant_pid)
            print("Real delegated cgroup v2: setsid descendant terminated after guardian SIGKILL")
        finally:
            if guardian.poll() is None:
                guardian.kill()
                guardian.wait(timeout=5)
            if descendant_pid is not None:
                try:
                    os.kill(descendant_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


def main():
    assert sys.platform == "linux"
    root = os.environ.get("FLOWERLY_PTY_CGROUP_ROOT")
    require_explicit_delegation(root)
    if root:
        exercise_real_cgroup(root)


if __name__ == "__main__":
    main()

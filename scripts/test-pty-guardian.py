#!/usr/bin/env python3
import base64
import json
import os
import re
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time

HELPER = Path(__file__).resolve().parent / "pty-guardian.py"

def exchange(path, payload):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(4)
        conn.connect(str(path))
        conn.sendall(json.dumps(payload).encode() + b"\n")
        result = conn.makefile("rb").readline()
    reply = json.loads(result)
    assert "error" not in reply, reply
    return reply


def test_same_uid_other_pid_denied(path):
    probe = subprocess.run(
        [sys.executable, "-c",
         "import socket,sys; s=socket.socket(socket.AF_UNIX); "
         "s.settimeout(3); s.connect(sys.argv[1]); "
         "s.sendall(b'{\"op\":\"terminate\"}\\n'); "
         "print(s.recv(4096).decode().strip())",
         str(path)],
        capture_output=True, text=True, timeout=5, check=True,
    )
    assert json.loads(probe.stdout) == {"error": "unauthorized"}, probe.stdout

def test_forced_guardian_death(root):
    crash_socket = Path(root) / "crash.sock"
    process = subprocess.Popen(
        [sys.executable, str(HELPER), str(crash_socket), "/bin/sh", str(os.getpid())],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(100):
            if crash_socket.exists():
                break
            assert process.poll() is None, "guardian died at startup"
            time.sleep(.02)
        else:
            raise AssertionError("guardian startup timed out")
        child = exchange(crash_socket, {"op": "status"})["pid"]
        assert child > 0
        command = b"sleep 60 & echo FLOWERLY_DESCENDANT_PID:$!\n"
        assert exchange(crash_socket, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
        for _ in range(100):
            output = exchange(crash_socket, {"op": "read", "cursor": 0})
            observed = re.search(rb"FLOWERLY_DESCENDANT_PID:(\d+)",
                                 base64.b64decode(output["output_base64"]))
            if observed:
                break
            time.sleep(.02)
        else:
            raise AssertionError("background job PID not observed")
        background = int(observed.group(1))
        with open(f"/proc/{background}/stat", encoding="utf-8") as child_stat:
            assert child_stat.read().split(") ", 1)[1][0] != "Z"
        process.kill()
        process.wait(timeout=3)
        for _ in range(100):
            try:
                with open(f"/proc/{child}/stat", encoding="utf-8") as child_stat:
                    status = child_stat.read().split(") ", 1)[1][0]
            except (FileNotFoundError, ProcessLookupError):
                break
            if status in ("Z", "X"):
                break
            time.sleep(.02)
        else:
            raise AssertionError("PTY child survived guardian SIGKILL")
        for _ in range(100):
            try:
                with open(f"/proc/{background}/stat", encoding="utf-8") as bg_stat:
                    status = bg_stat.read().split(") ", 1)[1][0]
            except (FileNotFoundError, ProcessLookupError):
                break
            if status in ("Z", "X"):
                break
            time.sleep(.02)
        else:
            raise AssertionError("background job survived guardian SIGKILL")
        print("PTY crash: direct shell and background process terminated")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=3)



def malformed_request(path, payload):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(3)
        conn.connect(str(path))
        conn.sendall(payload + b"\n")
        return json.loads(conn.makefile("rb").readline())


def test_malformed_commands_are_isolated(path):
    for payload in (b"[]", b"null", b"42", b"true", b"\xff",
                    b"{not json}", b'{"op":"read","cursor":true}',
                    b'{"op":"write","data_hex":"not hex"}'):
        result = malformed_request(path, payload)
        assert isinstance(result.get("error"), str), (payload, result)
        status = exchange(path, {"op": "status"})
        assert status["running"] is True, (payload, status)


def test_shell_limits(path):
    command = b"python3 -c 'import resource; print(\"FLOWERLY_LIMITS:%s:%s:%s\" % (resource.getrlimit(resource.RLIMIT_NOFILE)[0], resource.getrlimit(resource.RLIMIT_CORE)[0], resource.getrlimit(resource.RLIMIT_FSIZE)[0]))'\n"
    assert exchange(path, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
    for _ in range(100):
        reply = exchange(path, {"op": "read", "cursor": 0})
        output = base64.b64decode(reply["output_base64"])
        match = re.search(rb"FLOWERLY_LIMITS:(\d+):(\d+):(\d+)", output)
        if match:
            fd, core, fsize = map(int, match.groups())
            assert fd <= 256 and core == 0 and fsize <= 128 * 1024 * 1024
            return
        time.sleep(.02)
    raise AssertionError("PTY child limits could not be observed")

def test_root_shell_refusal():
    assert os.geteuid() == 0, "root refusal must be tested as root"
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-root-refusal-") as root:
        path = Path(root) / "root.sock"
        result = subprocess.run(
            [sys.executable, str(HELPER), str(path), "/bin/sh", str(os.getpid())],
            env=dict(os.environ, FLOWERLY_PTY_ISOLATION="development"),
            capture_output=True, timeout=5,
        )
        assert result.returncode != 0, "root-owned guardian unexpectedly started"
        assert b"refusing root-owned PTY shell; run as an unprivileged user" in result.stderr, result.stderr
        assert not path.exists(), "root-owned guardian created a control socket"
    print("PTY guardian: root-owned shell startup rejected before control socket creation")


def main():
    assert sys.platform == "linux"
    assert os.geteuid() != 0, "interactive PTY regressions require an unprivileged Linux UID"
    print(f"PTY guardian positive tests: EUID={os.geteuid()}", flush=True)
    os.environ["FLOWERLY_PTY_ISOLATION"] = "development"
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-guardian-") as root:
        test_forced_guardian_death(root)
        path = Path(root) / "supervisor.sock"
        supervisor = subprocess.Popen(
            [sys.executable, str(HELPER), str(path), "/bin/sh", str(os.getpid())],
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        try:
            for _ in range(100):
                if path.exists():
                    break
                assert supervisor.poll() is None, "guardian exited before readiness"
                time.sleep(.02)
            else:
                raise AssertionError("guardian startup timed out")
            test_same_uid_other_pid_denied(path)
            test_malformed_commands_are_isolated(path)
            test_shell_limits(path)
            first = exchange(path, {"op": "attach", "cursor": 0})
            pid = first["pid"]
            assert first["running"] is True and pid > 0
            assert exchange(path, {"op": "resize", "rows": 41, "cols": 93})["cols"] == 93
            data = b"echo FLOWERLY_PTY_STABLE\n"
            assert exchange(path, {"op": "write", "data_hex": data.hex()})["written"] == len(data)
            for _ in range(100):
                later = exchange(path, {"op": "read", "cursor": first["cursor"]})
                if b"FLOWERLY_PTY_STABLE" in base64.b64decode(later["output_base64"]):
                    break
                time.sleep(.02)
            else:
                raise AssertionError("child output not returned after reconnect")
            second = exchange(path, {"op": "attach", "cursor": 0})
            assert second["pid"] == pid and second["running"] is True
            assert b"FLOWERLY_PTY_STABLE" in base64.b64decode(second["output_base64"])
            noisy = b"python3 -c 'import sys; sys.stdout.write(\"Q\"*150000); sys.stdout.flush()'\n"
            assert exchange(path, {"op": "write", "data_hex": noisy.hex()})["written"] == len(noisy)
            for _ in range(100):
                burst = exchange(path, {"op": "read", "cursor": 0})
                if burst["cursor"] > 150000 and burst["truncated"]:
                    assert len(base64.b64decode(burst["output_base64"])) <= 65536
                    break
                time.sleep(.02)
            else:
                raise AssertionError("bounded replay/truncation not observed for noisy PTY")
            follow_cursor = exchange(path, {"op": "status"})["cursor"]
            command = b"echo FLOWERLY_INCREMENTAL_FOLLOW\n"
            assert exchange(path, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
            for _ in range(80):
                frame = exchange(path, {"op": "follow", "cursor": follow_cursor, "wait_ms": 100})
                if b"FLOWERLY_INCREMENTAL_FOLLOW" in base64.b64decode(frame["output_base64"]):
                    break
                time.sleep(.02)
            else:
                raise AssertionError("PTY incremental follow did not deliver new output")
            invalid_wait = malformed_request(path, b'{"op":"follow","cursor":0,"wait_ms":999}')
            assert invalid_wait.get("error") == "invalid wait_ms", invalid_wait
            assert exchange(path, {"op": "terminate"})["terminated"] is True
            assert supervisor.wait(timeout=5) == 0
            print("PTY guardian: same live PID, reconnect, bounded replay, I/O, resize, terminate")
        finally:
            if supervisor.poll() is None:
                supervisor.terminate()
                try:
                    supervisor.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    supervisor.kill()
                    supervisor.wait(timeout=3)

if __name__ == "__main__":
    if sys.argv[1:] == ["--expect-root-denied"]:
        test_root_shell_refusal()
    elif len(sys.argv) == 1:
        main()
    else:
        raise SystemExit("usage: test-pty-guardian.py [--expect-root-denied]")

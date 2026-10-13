#!/usr/bin/env python3
"""Real-process black-box PTY ownership/reattach/resize/termination test."""
import base64
import json
import os
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

def main():
    assert sys.platform == "linux"
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-guardian-") as root:
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
    main()

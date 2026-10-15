#!/usr/bin/env python3
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


def receive_exact(connection, length):
    result = bytearray()
    while len(result) < length:
        next_bytes = connection.recv(length - len(result))
        if not next_bytes:
            raise AssertionError("unexpected end of IPC response")
        result.extend(next_bytes)
    return bytes(result)


def command(socket_path, name, args):
    identity = str(uuid.uuid4())
    request = json.dumps({
        "version": 1, "id": identity, "type": "command",
        "name": name, "args": args,
    }).encode()
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(5)
        connection.connect(str(socket_path))
        connection.sendall(struct.pack("<I", len(request)) + request)
        size = struct.unpack("<I", receive_exact(connection, 4))[0]
        response = json.loads(receive_exact(connection, size))
    assert response["id"] == identity, response
    return response


def main(binary_directory):
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-unavailable-") as temporary:
        work = Path(temporary)
        runtime = work / "runtime"
        state = work / "state"
        invalid_delegation = work / "ordinary-directory"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        invalid_delegation.mkdir(mode=0o700)
        environment = dict(
            os.environ,
            AGENTIC_RUNTIME_DIR=str(runtime),
            AGENTIC_STATE_DIR=str(state),
            FLOWERLY_PTY_ISOLATION="required",
            FLOWERLY_PTY_CGROUP_ROOT=str(invalid_delegation),
        )
        process = subprocess.Popen(
            [str(binary_directory / "flowerlyd")],
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        socket_path = runtime / "flowerlyd.sock"
        try:
            for _ in range(150):
                if process.poll() is not None:
                    raise AssertionError("flowerlyd exited without PTY delegation")
                if socket_path.exists():
                    try:
                        reply = command(socket_path, "session.list", {})
                        if reply["type"] == "ok":
                            break
                    except (OSError, AssertionError):
                        pass
                time.sleep(0.04)
            else:
                raise AssertionError("metadata daemon not ready")
            metadata = command(socket_path, "session.create", {})
            assert metadata["type"] == "ok", metadata
            terminated = command(
                socket_path, "session.terminate", {"id": metadata["data"]["id"]}
            )
            assert terminated["type"] == "ok", terminated
            denied = command(socket_path, "session.spawn", {"shell": "/bin/sh"})
            assert denied["type"] == "err", denied
            assert denied["code"] == "unavailable", denied
            assert "cgroup isolation unavailable" in denied["message"], denied
            assert command(socket_path, "session.list", {})["type"] == "ok"
            print("PTY cgroup unavailable: metadata ready, supervised spawn denied")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            if process.stdout is not None:
                process.stdout.close()


if __name__ == "__main__":
    assert sys.platform == "linux"
    main(Path(sys.argv[1]).resolve())

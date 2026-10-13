#!/usr/bin/env python3
"""Linux PTY guardian: peer-credential gated IPC with bounded output replay.

This independent helper is the process-ownership substrate for #50. It is not
yet wired into sessiond; a Session database row never proves child ownership.
"""
import base64
import errno
import fcntl
import json
import os
import select
import signal
import socket
import struct
import sys
import termios
import time

REPLAY_LIMIT = 65536
REQUEST_LIMIT = 32768

def supervise(path, shell, parent_pid):
    if shell not in ("/bin/sh", "/bin/bash") or os.path.lexists(path):
        raise ValueError("shell not allowlisted or socket already exists")
    pid, fd = os.forkpty()
    if pid == 0:
        os.execv(shell, [shell, "-i"])
    os.set_blocking(fd, False)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path)
    os.chmod(path, 0o600)
    listener.listen(8)
    listener.setblocking(False)
    identity = os.stat(path)
    replay = bytearray()
    first, cursor, exit_code = 0, 0, None

    def harvest():
        nonlocal first, cursor
        while True:
            try:
                chunk = os.read(fd, 4096)
            except BlockingIOError:
                return
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return
                raise
            if not chunk:
                return
            replay.extend(chunk)
            cursor += len(chunk)
            if len(replay) > REPLAY_LIMIT:
                dropped = len(replay) - REPLAY_LIMIT
                del replay[:dropped]
                first += dropped

    def poll():
        nonlocal exit_code
        if exit_code is None:
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                exit_code = os.waitstatus_to_exitcode(status)

    def dispatch(command):
        nonlocal exit_code
        poll()
        operation = command.get("op")
        if operation in ("attach", "read", "status"):
            harvest()
            position = command.get("cursor", first)
            if type(position) is not int or not 0 <= position <= cursor:
                return {"error": "invalid cursor"}
            return {"pid": pid, "running": exit_code is None,
                    "exit_code": exit_code, "cursor": cursor,
                    "truncated": position < first,
                    "output_base64": base64.b64encode(replay[max(first, position) - first:]).decode()}
        if operation == "write":
            if exit_code is not None:
                return {"error": "exited"}
            data = command.get("data_hex")
            if not isinstance(data, str):
                return {"error": "data_hex required"}
            try:
                data = bytes.fromhex(data)
            except ValueError:
                return {"error": "bad hex"}
            if len(data) > 4096:
                return {"error": "write too large"}
            try:
                return {"written": os.write(fd, data)}
            except BlockingIOError:
                return {"error": "backpressure"}
        if operation == "resize":
            rows, cols = command.get("rows"), command.get("cols")
            if type(rows) is not int or type(cols) is not int or not (1 <= rows <= 1000 and 1 <= cols <= 1000):
                return {"error": "bad dimensions"}
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
            return {"rows": rows, "cols": cols}
        if operation == "terminate":
            if exit_code is None:
                os.killpg(pid, signal.SIGTERM)
                deadline = time.monotonic() + 1
                while time.monotonic() < deadline:
                    poll()
                    if exit_code is not None:
                        break
                    time.sleep(.02)
                if exit_code is None:
                    try:
                        os.killpg(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    _child, status = os.waitpid(pid, 0)
                    exit_code = os.waitstatus_to_exitcode(status)
            return {"terminated": True}
        return {"error": "unknown operation"}

    try:
        while os.getppid() == parent_pid:
            poll()
            ready, _, _ = select.select([listener, fd], [], [], .05)
            if fd in ready:
                harvest()
            if listener in ready:
                client, _ = listener.accept()
                with client:
                    client.settimeout(2)
                    _peer, uid, _gid = struct.unpack("3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    if uid != os.getuid():
                        client.sendall(b'{"error":"unauthorized"}\n')
                        continue
                    raw = bytearray()
                    while b"\n" not in raw and len(raw) <= REQUEST_LIMIT:
                        part = client.recv(4096)
                        if not part:
                            break
                        raw.extend(part)
                    try:
                        if len(raw) > REQUEST_LIMIT:
                            raise ValueError("oversized request")
                        command = json.loads(raw.split(b"\n", 1)[0])
                        reply = dispatch(command)
                    except (ValueError, KeyError, TypeError) as exc:
                        reply = {"error": str(exc)}
                    client.sendall(json.dumps(reply).encode() + b"\n")
                    if reply.get("terminated"):
                        break
    finally:
        listener.close()
        if exit_code is None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
        os.close(fd)
        try:
            current = os.stat(path)
            if (current.st_ino, current.st_dev) == (identity.st_ino, identity.st_dev):
                os.unlink(path)
        except FileNotFoundError:
            pass

if __name__ == "__main__":
    if sys.platform != "linux" or len(sys.argv) != 4:
        sys.exit("usage: pty-guardian.py SOCKET /bin/sh PARENT_PID (Linux only)")
    supervise(sys.argv[1], sys.argv[2], int(sys.argv[3]))

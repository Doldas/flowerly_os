#!/usr/bin/env python3
"""Linux PTY guardian: peer-credential gated IPC with bounded output replay.

This independent helper is the process-ownership substrate for #50. It is not
yet wired into sessiond; a Session database row never proves child ownership.
"""
import base64
import ctypes
import errno
import fcntl
import json
import os
import resource
import select
import signal
import socket
import struct
import sys
import termios
import time

REPLAY_LIMIT = 65536
REQUEST_LIMIT = 32768

def _kill_owned_terminal_session(shell_sid):
    """Best-effort group-independent cleanup; pidfds prevent signaling reused PIDs.

    This covers normal job-control background process groups sharing the TTY
    session. A child deliberately calling setsid() needs cgroup containment.
    """
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        return
    uid = os.getuid()
    until = time.monotonic() + 1.0
    while time.monotonic() < until:
        found = False
        for raw in os.listdir("/proc"):
            if not raw.isdigit():
                continue
            candidate = int(raw)
            if candidate == os.getpid():
                continue
            try:
                if os.stat("/proc/" + raw).st_uid != uid or os.getsid(candidate) != shell_sid:
                    continue
                fd = os.pidfd_open(candidate)
            except (ProcessLookupError, PermissionError, FileNotFoundError):
                continue
            try:
                # Do not signal a PID that changed identity or session between
                # discovering it and opening its pidfd.
                if os.getsid(candidate) == shell_sid:
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
                    found = True
            except (ProcessLookupError, PermissionError):
                pass
            finally:
                os.close(fd)
        if not found:
            break
        time.sleep(.02)


def _watch_guardian(shell_sid, parent_pid, ready_fd):
    """A separate process cleans up terminal job groups after guardian SIGKILL."""
    stopped = False

    def stop(_sig, _frame):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, stop)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
        os._exit(127)
    try:
        os.write(ready_fd, b"1")
    except OSError:
        os._exit(127)
    finally:
        os.close(ready_fd)
    while not stopped and os.getppid() == parent_pid:
        time.sleep(.05)
    _kill_owned_terminal_session(shell_sid)
    os._exit(0)


def supervise(path, shell, parent_pid):
    if shell not in ("/bin/sh", "/bin/bash") or os.path.lexists(path):
        raise ValueError("shell not allowlisted or socket already exists")
    if os.geteuid() == 0:
        # Until per-user UID delegation exists, a root sessiond must never
        # produce an interactive root shell through a normal Session request.
        raise PermissionError("refusing root-owned PTY shell; run as an unprivileged user")
    guardian_pid = os.getpid()
    release_read, release_write = os.pipe()
    ready_read, ready_write = os.pipe()
    pid, fd = os.forkpty()
    if pid == 0:
        os.close(release_write)
        os.close(ready_read)
        os.close(ready_write)
        libc = ctypes.CDLL(None, use_errno=True)
        # Kill our direct shell even when guardian SIGKILL skips finally.
        if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
            os._exit(127)
        if os.getppid() != guardian_pid or os.read(release_read, 1) != b"1":
            os._exit(127)
        os.close(release_read)
        # Prevent runaway file descriptors, unlimited files and core dumps.
        # Do not set RLIMIT_NPROC here: it is accounted per real UID and
        # could interfere with unrelated interactive sessions.
        resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_FSIZE, (128 * 1024 * 1024, 128 * 1024 * 1024))
        os.execv(shell, [shell, "-i"])
    os.close(release_read)
    watcher = os.fork()
    if watcher == 0:
        # Do not keep the PTY master open in the watcher.
        os.close(fd)
        os.close(release_write)
        os.close(ready_read)
        _watch_guardian(pid, guardian_pid, ready_write)
    os.close(ready_write)
    try:
        if os.read(ready_read, 1) != b"1":
            raise RuntimeError("PTY cleanup watcher failed to initialize")
    finally:
        os.close(ready_read)
    os.write(release_write, b"1")
    os.close(release_write)
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
        # A continuously noisy child must not starve terminal control requests.
        # Give each event-loop tick a bounded 64 KiB PTY-drain budget.
        budget = REPLAY_LIMIT
        while budget:
            try:
                chunk = os.read(fd, min(4096, budget))
            except BlockingIOError:
                return
            except OSError as exc:
                if exc.errno == errno.EIO:
                    return
                raise
            if not chunk:
                return
            budget -= len(chunk)
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
        if not isinstance(command, dict):
            return {"error": "PTY request must be a JSON object"}
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
                    peer_pid, uid, _gid = struct.unpack("3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    # An unrelated same-UID process must not control the guardian.
                    # Bind each accepted connection to the original sessiond PID.
                    if uid != os.getuid() or peer_pid != parent_pid:
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
                    except (ValueError, KeyError, TypeError, UnicodeError) as exc:
                        reply = {"error": str(exc)}
                    try:
                        client.sendall(json.dumps(reply).encode() + b"\n")
                    except (BrokenPipeError, ConnectionResetError):
                        # A disconnected controller cannot crash the guardian.
                        continue
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

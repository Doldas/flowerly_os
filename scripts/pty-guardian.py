#!/usr/bin/env python3

import base64
import ctypes
import errno
import fcntl
import json
import os
import resource
import re
import select
import signal
import socket
import struct
import sys
import termios
import time
import uuid

REPLAY_LIMIT = 65536
REQUEST_LIMIT = 32768


class SessionCgroup:
    def __init__(self):
        self.mode = os.environ.get("FLOWERLY_PTY_ISOLATION", "required")
        self.parent_fd = None
        self.group_fd = None
        self.name = None
        if self.mode == "development":
            return
        if self.mode != "required":
            raise ValueError("invalid PTY isolation mode")
        root = os.environ.get("FLOWERLY_PTY_CGROUP_ROOT")
        if not root or not os.path.isabs(root):
            raise RuntimeError("delegated cgroup v2 root is required")
        self.parent_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            if not os.path.isfile("/sys/fs/cgroup/cgroup.controllers"):
                raise RuntimeError("Linux cgroup v2 is required")
            chosen_name = os.environ.get("FLOWERLY_PTY_CGROUP_NAME")
            if chosen_name is None:
                self.name = "pty-" + uuid.uuid4().hex
            elif re.fullmatch(r"pty-session-[0-9a-f]{32}", chosen_name):
                self.name = chosen_name
            else:
                raise ValueError("invalid supervised cgroup identity")
            os.mkdir(self.name, mode=0o700, dir_fd=self.parent_fd)
            self.group_fd = os.open(
                self.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=self.parent_fd,
            )
            self.write("pids.max", "64")
            self.write("memory.max", str(512 * 1024 * 1024))
            self.write("cpu.max", "100000 100000")
            if not self.read("cgroup.events"):
                raise RuntimeError("cgroup.events unavailable")
            if not self.exists("cgroup.kill"):
                raise RuntimeError("cgroup.kill unavailable")
        except BaseException:
            self.cleanup()
            raise

    def exists(self, name):
        try:
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=self.group_fd)
            os.close(fd)
            return True
        except OSError:
            return False

    def write(self, name, data):
        fd = os.open(name, os.O_WRONLY | os.O_NOFOLLOW, dir_fd=self.group_fd)
        try:
            value = data.encode("ascii")
            if os.write(fd, value) != len(value):
                raise OSError("incomplete cgroup write")
        finally:
            os.close(fd)

    def read(self, name):
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=self.group_fd)
        try:
            return os.read(fd, 8192).decode("ascii")
        finally:
            os.close(fd)

    def move(self, pid):
        if self.mode == "required":
            self.write("cgroup.procs", str(pid))

    def kill(self):
        if self.mode == "required":
            self.write("cgroup.kill", "1")

    def cleanup(self):
        if self.group_fd is not None:
            os.close(self.group_fd)
            self.group_fd = None
        if self.parent_fd is not None:
            if self.name:
                try:
                    os.rmdir(self.name, dir_fd=self.parent_fd)
                except OSError as error:
                    if error.errno not in (errno.ENOENT, errno.EBUSY, errno.ENOTEMPTY):
                        raise
            os.close(self.parent_fd)
            self.parent_fd = None

def _kill_owned_terminal_session(shell_sid):
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


def _watch_guardian(shell_sid, parent_pid, ready_fd, isolation):
    stopped = False

    def stop(_sig, _frame):
        nonlocal stopped
        stopped = True

    signal.signal(signal.SIGTERM, stop)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(1, signal.SIGTERM, 0, 0, 0) != 0:
        os._exit(127)
    if os.getppid() != parent_pid:
        stopped = True
    try:
        os.write(ready_fd, b"1")
    except OSError:
        os._exit(127)
    finally:
        os.close(ready_fd)
    while not stopped and os.getppid() == parent_pid:
        time.sleep(.05)
    if isolation.mode == "required":
        isolation.kill()
        isolation.cleanup()
    else:
        _kill_owned_terminal_session(shell_sid)
    os._exit(0)


def supervise(path, shell, parent_pid):
    if shell not in ("/bin/sh", "/bin/bash") or os.path.lexists(path):
        raise ValueError("shell not allowlisted or socket already exists")
    if os.geteuid() == 0:
        raise PermissionError("refusing root-owned PTY shell; run as an unprivileged user")
    isolation = SessionCgroup()
    guardian_pid = os.getpid()
    release_read, release_write = os.pipe()
    ready_read, ready_write = os.pipe()
    pid, fd = os.forkpty()
    if pid == 0:
        os.close(release_write)
        os.close(ready_read)
        os.close(ready_write)
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(1, signal.SIGKILL, 0, 0, 0) != 0:
            os._exit(127)
        if os.getppid() != guardian_pid or os.read(release_read, 1) != b"1":
            os._exit(127)
        os.close(release_read)
        resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_FSIZE, (128 * 1024 * 1024, 128 * 1024 * 1024))
        os.execv(shell, [shell, "-i"])
    os.close(release_read)
    try:
        isolation.move(pid)
    except BaseException:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        isolation.cleanup()
        raise
    watcher = os.fork()
    if watcher == 0:
        os.close(fd)
        os.close(release_write)
        os.close(ready_read)
        _watch_guardian(pid, guardian_pid, ready_write, isolation)
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
        if operation == "lifecycle":
            return {"pid": pid, "running": exit_code is None, "exit_code": exit_code}
        if operation in ("attach", "read", "status", "follow"):
            harvest()
            position = command.get("cursor", first)
            if type(position) is not int or not 0 <= position <= cursor:
                return {"error": "invalid cursor"}
            if operation == "follow":
                wait_ms = command.get("wait_ms", 100)
                if type(wait_ms) is not int or not 0 <= wait_ms <= 250:
                    return {"error": "invalid wait_ms"}
                if position == cursor and exit_code is None and wait_ms:
                    ready, _, _ = select.select([fd], [], [], wait_ms / 1000)
                    if fd in ready:
                        harvest()
                    poll()
            return {"pid": pid, "running": exit_code is None,
                    "isolation": isolation.mode,
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
            if isolation.mode == "required":
                isolation.kill()
            if exit_code is None and isolation.mode == "development":
                os.killpg(pid, signal.SIGTERM)
                deadline = time.monotonic() + 1
                while time.monotonic() < deadline:
                    poll()
                    if exit_code is not None:
                        break
                    time.sleep(.02)
                if exit_code is None:
                    if isolation.mode == "development":
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
                    if uid != os.getuid() or peer_pid != parent_pid:
                        client.sendall(b'{"error":"unauthorized"}\n')
                        continue
                    raw = bytearray()
                    try:
                        while b"\n" not in raw and len(raw) <= REQUEST_LIMIT:
                            part = client.recv(4096)
                            if not part:
                                break
                            raw.extend(part)
                    except (socket.timeout, ConnectionResetError, BrokenPipeError):
                        continue
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
                        continue
                    if reply.get("terminated"):
                        break
    finally:
        listener.close()
        if isolation.mode == "required":
            isolation.kill()
        if exit_code is None:
            if isolation.mode == "development":
                try:
                    os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
        os.close(fd)
        isolation.cleanup()
        try:
            os.waitpid(watcher, os.WNOHANG)
        except ChildProcessError:
            pass
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

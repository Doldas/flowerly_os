#!/usr/bin/env python3

import base64
import ctypes
import errno
import fcntl
import json
import os
import pwd
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
PR_SET_NO_NEW_PRIVS = 38


def shell_credentials():
    """Explicit opt-in only; legacy same-UID mode stays fail-closed."""
    raw = os.environ.get("FLOWERLY_PTY_SHELL_UID")
    if raw is None:
        return None
    if not raw.isdecimal() or raw.startswith("+"):
        raise ValueError("invalid PTY shell UID")
    uid = int(raw)
    if uid == 0 or uid == os.geteuid():
        raise PermissionError("PTY shell must have a separate non-root UID")
    account = pwd.getpwuid(uid)
    if account.pw_gid == 0:
        raise PermissionError("PTY shell primary group must not be root")
    return uid, account.pw_gid


def drop_to_shell(credentials):
    """Drop groups, GID, UID, and future privilege escalation before exec."""
    uid, gid = credentials
    os.setgroups([])
    os.setresgid(gid, gid, gid)
    os.setresuid(uid, uid, uid)
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_NO_NEW_PRIVS failed")
    if os.getresuid() != (uid, uid, uid) or os.getresgid() != (gid, gid, gid):
        raise RuntimeError("PTY shell credentials not fully dropped")
    if os.getgroups():
        raise RuntimeError("PTY shell inherited supplementary groups")


def verify_shell_migration_boundary(credentials, root_fd):
    """Kernel open probe with shell credentials, no writes or PID migration."""
    targets = (("cgroup.procs", root_fd),)
    management_fd = os.open(
        "management", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
        dir_fd=root_fd,
    )
    try:
        targets += (("cgroup.procs", management_fd),)
        child = os.fork()
        if child == 0:
            try:
                drop_to_shell(credentials)
                for name, directory in targets:
                    try:
                        fd = os.open(name, os.O_WRONLY | os.O_NOFOLLOW, dir_fd=directory)
                    except OSError as error:
                        if error.errno not in (errno.EPERM, errno.EACCES):
                            os._exit(10)
                    else:
                        os.close(fd)
                        os._exit(11)
                os._exit(0)
            except BaseException:
                os._exit(12)
        _, status = os.waitpid(child, 0)
        if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
            raise RuntimeError(
                "unsafe or unverifiable PTY shell cgroup migration boundary"
            )
    finally:
        os.close(management_fd)



class SessionCgroup:
    def __init__(self):
        self.mode = os.environ.get("FLOWERLY_PTY_ISOLATION", "required")
        self.parent_fd = None
        self.group_fd = None
        self.shell_creds = shell_credentials()
        self.name = None
        if self.mode == "development":
            return
        if self.mode != "required":
            raise ValueError("invalid PTY isolation mode")
        root = os.environ.get("FLOWERLY_PTY_CGROUP_ROOT")
        if not root or not os.path.isabs(root):
            raise RuntimeError("delegated cgroup v2 root is required")
        chosen_name = os.environ.get("FLOWERLY_PTY_CGROUP_NAME")
        if chosen_name is None:
            self.name = "pty-" + uuid.uuid4().hex
        elif re.fullmatch(r"pty-session-[0-9a-f]{32}", chosen_name):
            self.name = chosen_name
        else:
            raise ValueError("invalid supervised cgroup identity")
        self.parent_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            if not os.path.isfile("/sys/fs/cgroup/cgroup.controllers"):
                raise RuntimeError("Linux cgroup v2 is required")
            if os.fstat(self.parent_fd).st_dev != os.stat("/sys/fs/cgroup").st_dev:
                raise RuntimeError("delegated root must reside on the cgroup v2 mount")
            # Reject the known unsafe delegation before creating any child.
            # This is not a substitute for a distinct-UID broker or its
            # kernel-enforced lifecycle and migration ownership.
            if self.shell_creds is None:
                self.reject_writable_escape_targets()
            else:
                verify_shell_migration_boundary(self.shell_creds, self.parent_fd)
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
            # cgroup.kill is kernel-owned and WRITE-ONLY (mode 0200 on cgroup v2).
            # A read-only existence probe incorrectly rejects a valid delegated
            # cgroup for an unprivileged service UID. Opening for writing is
            # non-destructive: only writing "1" kills the group.
            self.require_writable("cgroup.kill")
        except BaseException:
            self.cleanup()
            raise

    def reject_writable_escape_targets(self):
        """Conservatively refuse writable migration paths for the shell UID.

        This does not prove security against future same-UID sibling creation:
        the delegated parent and trusted management paths must be isolated by
        a separately privileged broker before required PTY is enabled.
        """
        for directory, name in ((self.parent_fd, "cgroup.procs"),):
            try:
                fd = os.open(name, os.O_WRONLY | os.O_NOFOLLOW, dir_fd=directory)
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EPERM):
                    raise RuntimeError(
                        f"unverifiable parent cgroup migration boundary: {error}"
                    ) from error
            else:
                os.close(fd)
                raise RuntimeError(
                    "unsafe PTY cgroup delegation: parent cgroup.procs is "
                    "writable by session UID"
                )

        try:
            management_fd = os.open(
                "management",
                os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                dir_fd=self.parent_fd,
            )
        except OSError as error:
            raise RuntimeError(
                f"trusted management cgroup missing or inaccessible: {error}"
            ) from error
        try:
            try:
                fd = os.open(
                    "cgroup.procs", os.O_WRONLY | os.O_NOFOLLOW,
                    dir_fd=management_fd,
                )
            except OSError as error:
                if error.errno not in (errno.EACCES, errno.EPERM):
                    raise RuntimeError(
                        f"unverifiable management cgroup boundary: {error}"
                    ) from error
            else:
                os.close(fd)
                raise RuntimeError(
                    "unsafe PTY cgroup delegation: management cgroup.procs "
                    "is writable by session UID"
                )
        finally:
            os.close(management_fd)

    def require_writable(self, name):
        try:
            fd = os.open(name, os.O_WRONLY | os.O_NOFOLLOW, dir_fd=self.group_fd)
        except OSError as exc:
            # Fail closed if cgroup.kill does not exist, is a symlink, or
            # cannot be opened for writing by the guardian's actual UID.
            raise RuntimeError(
                f"{name} unavailable or not writable (errno={exc.errno}: {exc.strerror})"
            ) from exc
        else:
            os.close(fd)

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
                for attempt in range(100):
                    try:
                        os.rmdir(self.name, dir_fd=self.parent_fd)
                        break
                    except OSError as error:
                        if error.errno == errno.ENOENT:
                            break
                        if error.errno not in (errno.EBUSY, errno.ENOTEMPTY):
                            raise
                        if attempt == 99:
                            raise RuntimeError("delegated PTY cgroup remained populated")
                        time.sleep(0.02)
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
        if isolation.shell_creds is not None:
            try:
                drop_to_shell(isolation.shell_creds)
            except BaseException:
                os._exit(127)
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

#!/usr/bin/env python3
"""Fail-closed cgroup broker foundation; never launches or migrates a shell.

Run as a dedicated *non-root* trusted UID with a separately delegated cgroup.
The caller's UID must be different. No arbitrary PID or path is accepted.
The Unix socket is private until independently reviewed systemd socket
activation and cross-UID credential authorization are available.
"""
import json
import os
import re
import socket
import struct
import sys

NAME = re.compile(r"pty-session-[0-9a-f]{32}\Z")
LIMITS = {"pids.max": "64", "memory.max": str(512 * 1024 * 1024), "cpu.max": "100000 100000"}
MAX_REQUEST = 4096
MAX_SESSIONS = 64


def read_request(conn):
    """Accept exactly one bounded, newline-terminated JSON object."""
    chunks = bytearray()
    while len(chunks) <= MAX_REQUEST:
        part = conn.recv(MAX_REQUEST + 1 - len(chunks))
        if not part:
            raise ValueError("incomplete request")
        chunks.extend(part)
        if b"\n" in chunks:
            line, tail = chunks.split(b"\n", 1)
            if tail or not line or len(line) > MAX_REQUEST:
                raise ValueError("extra bytes or invalid request length")
            return json.loads(line)
    raise ValueError("request too large")


def peer_uid(sock):
    data = sock.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
    return struct.unpack("3i", data)[1]


def write_control(directory_fd, name, value):
    fd = os.open(name, os.O_WRONLY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=directory_fd)
    try:
        payload = value.encode("ascii")
        if os.write(fd, payload) != len(payload):
            raise OSError("incomplete cgroup control write")
    finally:
        os.close(fd)


class CgroupBroker:
    def __init__(self, root, authorized_uid):
        if os.geteuid() == 0:
            raise PermissionError("root-owned PTY broker forbidden")
        if os.geteuid() == authorized_uid:
            raise PermissionError("broker and client must have separate UIDs")
        if not os.path.isfile("/sys/fs/cgroup/cgroup.controllers"):
            raise RuntimeError("unified cgroup v2 required")
        self.root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        if os.fstat(self.root_fd).st_dev != os.stat("/sys/fs/cgroup").st_dev:
            os.close(self.root_fd)
            raise RuntimeError("root is not on cgroup v2 mount")
        self.authorized_uid = authorized_uid
        self.owned = set()
        # In-memory ownership is lost on SIGKILL. Never adopt or overwrite
        # stale PTY groups without independently verified recovery authority.
        try:
            names = os.listdir(self.root_fd)
            if any(NAME.fullmatch(name) for name in names):
                raise RuntimeError("unreconciled PTY cgroups: broker startup denied")
        except BaseException:
            os.close(self.root_fd)
            raise

    def close(self):
        os.close(self.root_fd)

    def execute(self, request, uid):
        if uid != self.authorized_uid:
            raise PermissionError("unauthorized caller")
        if type(request) is not dict or set(request) != {"op", "name"}:
            raise ValueError("invalid request")
        op, name = request["op"], request["name"]
        if type(op) is not str or type(name) is not str or not NAME.fullmatch(name):
            raise ValueError("invalid operation or session identity")
        if op == "reserve":
            if len(self.owned) >= MAX_SESSIONS:
                raise RuntimeError("session quota exhausted")
            if name in self.owned:
                raise ValueError("already reserved")
            os.mkdir(name, 0o700, dir_fd=self.root_fd)
            fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=self.root_fd)
            try:
                for control, value in LIMITS.items():
                    write_control(fd, control, value)
            except BaseException:
                os.close(fd)
                os.rmdir(name, dir_fd=self.root_fd)
                raise
            os.close(fd)
            self.owned.add(name)
            return {"reserved": name}
        if op == "reap":
            if name not in self.owned:
                raise PermissionError("unowned session")
            fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=self.root_fd)
            try:
                write_control(fd, "cgroup.kill", "1")
            finally:
                os.close(fd)
            os.rmdir(name, dir_fd=self.root_fd)
            self.owned.remove(name)
            return {"reaped": name}
        raise ValueError("unsupported operation")


def serve(socket_path, root, authorized_uid):
    broker = CgroupBroker(root, authorized_uid)
    listener = socket.socket(socket.AF_UNIX)
    try:
        listener.bind(socket_path)
        os.chmod(socket_path, 0o600)
        listener.listen(4)
        while True:
            conn, _ = listener.accept()
            with conn:
                try:
                    conn.settimeout(2)
                    uid = peer_uid(conn)
                    message = read_request(conn)
                    response = broker.execute(message, uid)
                except (OSError, ValueError, RuntimeError, PermissionError) as error:
                    response = {"error": type(error).__name__}
                conn.sendall(json.dumps(response).encode("ascii") + b"\n")
    finally:
        broker.close()
        listener.close()
        os.unlink(socket_path)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: pty-cgroup-broker.py SOCKET DELEGATED_ROOT AUTHORIZED_UID")
    serve(sys.argv[1], sys.argv[2], int(sys.argv[3]))

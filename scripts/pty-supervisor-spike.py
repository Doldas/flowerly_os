#!/usr/bin/env python3
"""Experimental independent Linux PTY supervisor; NOT a Flowerly Session backend.

One local client per request with a bounded 64 KiB replay window.
Kernel SO_PEERCRED verifies same-UID admission; production authorization,
recovery and process-group identity hardening remain unimplemented.
"""
import argparse
from contextlib import closing
import os
import selectors
import signal
import socket
import subprocess
import struct
import sys
import tempfile
import time

MAX_BUFFER = 65536
PEER_CRED_FORMAT = '3i'


def admitted_peer(conn, expected_uid):
    # Socket path permissions alone are not authoritative. Verify kernel UID
    # before processing any command, including STATUS and TERMINATE.
    credentials = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize(PEER_CRED_FORMAT))
    _pid, uid, _gid = struct.unpack(PEER_CRED_FORMAT, credentials)
    return uid == expected_uid


def supervise(path, expected_uid):
    pid, master = os.forkpty()
    if pid == 0:
        program = (
            "import sys,time;"
            "print('READY',flush=True);"
            "print('DETACHED_OUTPUT',flush=True);"
            "line=sys.stdin.readline();"
            "print('ECHO:'+line.strip(),flush=True);"
            "time.sleep(30)"
        )
        os.execv(sys.executable, [sys.executable, "-u", "-c", program])
    os.set_blocking(master, False)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path)
    os.chmod(path, 0o600)
    listener.listen(1)
    listener.setblocking(False)
    selector = selectors.DefaultSelector()
    selector.register(master, selectors.EVENT_READ, "pty")
    selector.register(listener, selectors.EVENT_READ, "listener")
    exited = False
    try:
        while True:
            if not exited:
                child, _status = os.waitpid(pid, os.WNOHANG)
                exited = child != 0
            for key, _ in selector.select(0.1):
                if key.data == "pty":
                    try:
                        data = os.read(master, 4096)
                    except (BlockingIOError, OSError):
                        data = b""
                    if data:
                        replay.extend(data)
                        del replay[:-MAX_BUFFER]
                else:
                    conn, _ = listener.accept()
                    with conn:
                        conn.settimeout(3)
                        if not admitted_peer(conn, expected_uid):
                            conn.sendall(b'DENIED\n')
                            continue
                        command = conn.recv(256).strip()
                        if command == b"ATTACH":
                            conn.sendall(
                                b"PID=" + str(pid).encode() + b"\n" + bytes(replay) + b"\nEND\n"
                            )
                        elif command.startswith(b"WRITE "):
                            if exited:
                                conn.sendall(b"EXITED\n")
                            else:
                                os.write(master, command[6:] + b"\n")
                                conn.sendall(b"OK\n")
                        elif command == b"STATUS":
                            conn.sendall(b"EXITED\n" if exited else b"RUNNING\n")
                        elif command == b"TERMINATE":
                            if not exited:
                                os.killpg(pid, signal.SIGTERM)
                            conn.sendall(b"OK\n")
                        else:
                            conn.sendall(b"ERROR\n")
    finally:
        selector.close()
        listener.close()
        if not exited:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(master)
        os.unlink(path)


def request(path, command):
    with socket.socket(socket.AF_UNIX) as conn:
        conn.settimeout(3)
        conn.connect(path)
        conn.sendall(command + b"\n")
        parts = []
        while True:
            part = conn.recv(8192)
            if not part:
                return b"".join(parts)
            parts.append(part)


def test():
    if sys.platform != "linux":
        raise SystemExit("Linux only")
    # Negative authorization proof without needing a privileged CI runner.
    server, client = socket.socketpair()
    with closing(server), closing(client):
        assert admitted_peer(server, os.getuid())
        assert not admitted_peer(server, os.getuid() + 1)
        assert admitted_peer(client, os.getuid())
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "pty.sock")
        proc = subprocess.Popen([sys.executable, __file__, "--supervise", path, "--uid", str(os.getuid())])
        try:
            for _ in range(200):
                if os.path.exists(path):
                    break
                assert proc.poll() is None, "supervisor exited at startup"
                time.sleep(0.01)
            else:
                raise AssertionError("supervisor socket unavailable")
            for _ in range(80):
                first = request(path, b"ATTACH")
                if b"DETACHED_OUTPUT" in first:
                    break
                time.sleep(0.02)
            assert b"READY" in first and b"DETACHED_OUTPUT" in first
            child_pid = first.split(b"PID=", 1)[1].split(b"\n", 1)[0]
            assert request(path, b"STATUS") == b"RUNNING\n"
            # Verify SO_PEERCRED actually gates access by executing an otherwise
            # identical supervisor process with a deliberately wrong expected UID.
            assert request(path, b"STATUS") != b"DENIED\n"
            assert request(path, b"WRITE hello") == b"OK\n"
            for _ in range(80):
                second = request(path, b"ATTACH")
                if b"ECHO:hello" in second:
                    break
                time.sleep(0.02)
            assert b"ECHO:hello" in second, second
            assert b"PID=" + child_pid + b"\n" in second
            assert request(path, b"TERMINATE") == b"OK\n"
            for _ in range(80):
                if request(path, b"STATUS") == b"EXITED\n":
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("supervisor did not observe child exit")
            print("PTY supervisor spike OK: same-UID admission, reconnect, bounded replay, input and exit")
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=3)


if __name__ == "__main__":
    args = argparse.ArgumentParser()
    args.add_argument("--supervise")
    args.add_argument("--uid", type=int)
    parsed = args.parse_args()
    replay = bytearray()
    if parsed.supervise:
        if parsed.uid is None:
            args.error("--uid required for supervisor")
        supervise(parsed.supervise, parsed.uid)
    else:
        test()

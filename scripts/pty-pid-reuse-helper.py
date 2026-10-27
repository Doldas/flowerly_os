#!/usr/bin/env python3
"""Root-side helper for the #72 PID-reuse test on a disposable host.

Listens on a Unix socket that only CLIENT_UID may use. A request
{"pid": N} sets the init PID namespace's ns_last_pid to N - 1 and forks
an unrelated decoy (`sleep 300` as DECOY_USER) until one is born with
exactly PID N. It replies {"pid": N} or {"error": ...}. The helper never
signals anything; it only creates decoys, which it kills on exit.
"""
import json
import os
import pwd
import signal
import socket
import struct
import subprocess
import sys

LAST_PID = "/proc/sys/kernel/ns_last_pid"


def decoy(target, user):
    record = pwd.getpwnam(user)
    for _ in range(50):
        with open(LAST_PID, "w") as last:
            last.write(str(target - 1))
        process = subprocess.Popen(["sleep", "300"], user=record.pw_uid, group=record.pw_gid,
                                   extra_groups=[], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if process.pid == target:
            return process
        process.kill()
        process.wait()
    return None


def main(path, client_uid, user):
    if os.geteuid() != 0:
        raise SystemExit("pid-reuse helper must run as root on a disposable host")
    decoys = []
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path)
    os.chown(path, client_uid, -1)
    os.chmod(path, 0o600)
    listener.listen(4)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        while True:
            client, _ = listener.accept()
            with client:
                _pid, uid, _gid = struct.unpack(
                    "3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                if uid != client_uid:
                    continue
                try:
                    target = int(json.loads(client.recv(256))["pid"])
                    if not 300 < target < 4194304:
                        raise ValueError("pid out of range")
                    process = decoy(target, user)
                    if process is None:
                        reply = {"error": "could not reuse pid"}
                    else:
                        decoys.append(process)
                        reply = {"pid": process.pid}
                except (ValueError, KeyError, OSError) as error:
                    reply = {"error": str(error)}
                client.sendall(json.dumps(reply).encode())
    finally:
        for process in decoys:
            process.kill()
        os.unlink(path)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("usage: pty-pid-reuse-helper.py SOCKET CLIENT_UID DECOY_USER")
    main(sys.argv[1], int(sys.argv[2]), sys.argv[3])

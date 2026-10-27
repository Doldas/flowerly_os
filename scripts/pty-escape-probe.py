#!/usr/bin/env python3
"""Run INSIDE a Flowerly PTY shell by test-pty-containment-adversarial.py.

Tries to leave the Session cgroup with the shell's own credentials, also
from new user, mount and cgroup namespaces, and prints one JSON line:
{"marker": ..., "results": {attempt: outcome}, "cgroup": final cgroup}.
An outcome is "denied:<errno>", "escaped", "unavailable:<errno>" (the
kernel refused to create the namespace) or "contained" (a namespace view
that only shows the Session's own subtree).
"""
import ctypes
import json
import os
import sys
import tempfile

CLONE_NEWNS, CLONE_NEWCGROUP, CLONE_NEWUSER = 0x00020000, 0x02000000, 0x10000000
libc = ctypes.CDLL(None, use_errno=True)


def own_cgroup():
    with open("/proc/self/cgroup") as handle:
        return handle.read().strip().split("::", 1)[1]


def try_write(path):
    try:
        with open(path, "w") as handle:
            handle.write(str(os.getpid()))
        return "escaped" if own_cgroup() != START else "denied:noop"
    except OSError as error:
        return f"denied:{error.errno}"


def targets():
    session = "/sys/fs/cgroup" + START
    parent = os.path.dirname(session)
    found = {"parent": parent + "/cgroup.procs", "host_root": "/sys/fs/cgroup/cgroup.procs"}
    for name in sorted(os.listdir(parent)):
        if name.startswith("management") and os.path.isdir(os.path.join(parent, name)):
            found["sibling_" + name.split("-")[0]] = os.path.join(parent, name, "cgroup.procs")
    return found


def in_child(function):
    """Each namespace attempt runs in a child so the probe itself stays put."""
    read, write = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read)
        try:
            outcome = function()
        except BaseException as error:  # report, never crash the probe
            import traceback
            outcome = {"error": repr(error) + " at " + traceback.format_exc().splitlines()[-3].strip()}
        os.write(write, json.dumps(outcome).encode())
        os._exit(0)
    os.close(write)
    data = b""
    while chunk := os.read(read, 65536):
        data += chunk
    os.close(read)
    os.waitpid(pid, 0)
    return json.loads(data or b'{"error": "no result"}')


def enter_user_namespace(extra=0):
    uid, gid = os.getuid(), os.getgid()
    if libc.unshare(CLONE_NEWUSER | extra) != 0:
        return ctypes.get_errno()
    with open("/proc/self/setgroups", "w") as handle:
        handle.write("deny")
    with open("/proc/self/uid_map", "w") as handle:
        handle.write(f"0 {uid} 1")
    with open("/proc/self/gid_map", "w") as handle:
        handle.write(f"0 {gid} 1")
    return 0


def userns_writes():
    error = enter_user_namespace()
    if error:
        return {"userns": f"unavailable:{error}"}
    return {f"userns_{name}": try_write(path) for name, path in targets().items()}


def cgroupns_mount():
    error = enter_user_namespace(CLONE_NEWNS | CLONE_NEWCGROUP)
    if error:
        return {"cgroupns": f"unavailable:{error}"}
    mount = tempfile.mkdtemp()
    if libc.mount(b"none", mount.encode(), b"cgroup2", 0, None) != 0:
        return {"cgroupns_mount": f"denied:{ctypes.get_errno()}"}
    # The namespace root is the Session cgroup itself: no parent is visible.
    outcome = {"cgroupns_view": "contained" if own_cgroup() == "/" else "escaped"}
    try:
        names = os.listdir(mount)
        outcome["cgroupns_root"] = "escaped" if any(
            name.startswith(("management", "pty-")) for name in names) else "contained"
    except OSError as error:  # the Session cgroup is 0700 to the manager UID
        outcome["cgroupns_root"] = f"denied:{error.errno}"
    outcome["cgroupns_procs"] = try_write(os.path.join(mount, "cgroup.procs"))
    try:
        os.mkdir(os.path.join(mount, "escape"))
        outcome["cgroupns_mkdir"] = "escaped"
    except OSError as error:
        outcome["cgroupns_mkdir"] = f"denied:{error.errno}"
    return outcome


def setns_host():
    outcome = {}
    for kind, flag in (("cgroup", CLONE_NEWCGROUP), ("mnt", CLONE_NEWNS), ("user", CLONE_NEWUSER)):
        try:
            fd = os.open(f"/proc/1/ns/{kind}", os.O_RDONLY)
        except OSError as error:
            outcome[f"setns_{kind}"] = f"denied:{error.errno}"
            continue
        result = libc.setns(fd, flag)
        outcome[f"setns_{kind}"] = "escaped" if result == 0 else f"denied:{ctypes.get_errno()}"
        os.close(fd)
    return outcome


START = own_cgroup()
results = {name: try_write(path) for name, path in targets().items()}
for attempt in (userns_writes, cgroupns_mount, setns_host):
    results.update(in_child(attempt))
print(json.dumps({"marker": sys.argv[1], "results": results, "cgroup": own_cgroup(),
                  "start": START}), flush=True)

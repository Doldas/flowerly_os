#!/usr/bin/env python3
"""Confinement probe for the remote provider unit (ADR-0022, #263).

Runs INSIDE `flowerly-provider-remote@probe.service` on a disposable Arch
Linux/KVM guest, with the unit's ExecStart replaced by this script through a
drop-in, so every confinement setting of the real template applies. See
web-docs/docs/remote-provider-unit-test-plan.md.

Each check attempts one thing the provider must not be able to do and
records what happened. It never reads secret contents: a file check only
tries to open it, and the content is discarded unread. Output is one JSON
object; the exit status is 1 if any check found the boundary open.

Offline, run outside any sandbox, it must report violations: that is how
its own detection is tested (scripts/test-remote-provider-operator.py).
"""
import errno
import json
import os
import socket
import sys


def attempt(action):
    try:
        action()
        return "allowed"
    except OSError as err:
        return errno.errorcode.get(err.errno, str(err.errno))


def must_fail(name, action, results, accept=None):
    """Denied, and by a confinement errno when `accept` is given: a refused
    or timed-out connection proves nothing was blocked."""
    observed = attempt(action)
    ok = observed != "allowed" and (accept is None or observed in accept)
    results[name] = {"expected": sorted(accept) if accept else "denied", "observed": observed, "ok": ok}


# What the unit's own settings return, as opposed to "nobody answered".
BLOCKED = {"EPERM", "EACCES"}
FAMILY_BLOCKED = {"EAFNOSUPPORT", "EPERM", "EACCES"}


def open_and_discard(path, mode="rb"):
    def action():
        with open(path, mode) as handle:
            if "r" in mode:
                handle.read(0)
    return action


def listdir(path):
    return lambda: os.listdir(path)


def status_fields():
    fields = {}
    with open("/proc/self/status") as status:
        for line in status:
            key, _, value = line.partition(":")
            fields[key] = value.strip()
    return fields


def connect(family, address, timeout=2.0):
    def action():
        with socket.socket(family, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout)
            sock.connect(address)
    return action


def open_socket(family, kind=socket.SOCK_RAW, proto=0):
    def action():
        socket.socket(family, kind, proto).close()
    return action


def check_files(results, instance):
    # Flowerly state and every other credential store.
    for path in ["/var/lib/flowerly/state.db", "/var/lib/flowerly", "/run/flowerly",
                 "/etc/credstore", "/etc/credstore.encrypted", f"/etc/credstore.encrypted/{instance}.key",
                 "/etc/shadow", "/etc/gshadow", "/root", "/home", "/var/lib/flowerly-evolution",
                 "/var/lib/flowerly-provider-operator", "/var/log/flowerly-provider-acceptance"]:
        action = listdir(path) if not os.path.splitext(path)[1] and path not in ("/etc/shadow", "/etc/gshadow") \
            else open_and_discard(path)
        must_fail(f"read {path}", action, results)
    # Writes outside the runtime directory.
    for path in ["/etc/flowerly-probe", "/usr/local/flowerly-probe", "/var/lib/flowerly-probe",
                 "/run/flowerly-probe", "/var/tmp/../lib/flowerly-probe"]:
        must_fail(f"write {path}", open_and_discard(path, "wb"), results)


def check_credentials(results, instance):
    directory = os.environ.get("CREDENTIALS_DIRECTORY")
    names = sorted(os.listdir(directory)) if directory else []
    results["credentials directory holds only this instance's key"] = {
        "expected": [f"{instance}.key"], "observed": names, "ok": names == [f"{instance}.key"]}


def check_privileges(results):
    fields = status_fields()
    for key, expected in [("CapInh", "0000000000000000"), ("CapPrm", "0000000000000000"),
                          ("CapEff", "0000000000000000"), ("CapBnd", "0000000000000000"),
                          ("CapAmb", "0000000000000000"), ("NoNewPrivs", "1"), ("Seccomp", "2")]:
        observed = fields.get(key)
        results[f"status {key}"] = {"expected": expected, "observed": observed, "ok": observed == expected}
    uids = fields.get("Uid", "0").split()
    groups = fields.get("Groups", "").split()
    gid = fields.get("Gid", "0").split()
    results["not root"] = {"expected": "uid and gid != 0", "observed": uids + gid,
                           "ok": "0" not in uids + gid}
    results["no supplementary group"] = {"expected": f"subset of [{gid[1]}]", "observed": groups,
                                         "ok": all(g == gid[1] for g in groups)}
    must_fail("setuid(0)", lambda: os.setuid(0), results)
    must_fail("read /proc/1/environ", open_and_discard("/proc/1/environ"), results)
    must_fail("list /proc/1/fd", listdir("/proc/1/fd"), results)
    if hasattr(os, "unshare"):
        must_fail("unshare user namespace", lambda: os.unshare(os.CLONE_NEWUSER), results)
    must_fail("write /proc/sys/kernel/hostname", open_and_discard("/proc/sys/kernel/hostname", "w"), results)
    must_fail("open /dev/kvm", open_and_discard("/dev/kvm"), results)
    must_fail("open /dev/mem", open_and_discard("/dev/mem"), results)


def check_network(results, resolver):
    # Denied by the unit itself (IPAddressDeny, RestrictAddressFamilies),
    # whatever the application would do.
    for name, family, address in [
        ("connect 127.0.0.1:22", socket.AF_INET, ("127.0.0.1", 22)),
        ("connect [::1]:22", socket.AF_INET6, ("::1", 22)),
        ("connect 169.254.169.254:80 (metadata)", socket.AF_INET, ("169.254.169.254", 80)),
        ("connect [fe80::1%1]:80", socket.AF_INET6, ("fe80::1%1", 80, 0, 1)),
    ]:
        must_fail(name, connect(family, address), results, BLOCKED)
    must_fail("AF_PACKET socket", open_socket(socket.AF_PACKET), results, FAMILY_BLOCKED)
    must_fail("AF_NETLINK socket", open_socket(socket.AF_NETLINK, socket.SOCK_RAW, 0), results, FAMILY_BLOCKED)
    must_fail("raw ICMP socket", open_socket(socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_ICMP),
              results, FAMILY_BLOCKED)
    if resolver:
        # The loopback DNS stub the operator drop-in allows must stay usable.
        observed = attempt(connect(socket.AF_INET, (resolver, 53)))
        results[f"connect DNS stub {resolver}:53"] = {
            "expected": "allowed or refused by the stub", "observed": observed,
            "ok": observed in ("allowed", "ECONNREFUSED")}


def main(argv):
    instance = os.environ.get("FLOWERLY_PROBE_INSTANCE", "probe")
    resolver = os.environ.get("FLOWERLY_PROBE_RESOLVER", "")
    results = {}
    check_files(results, instance)
    check_credentials(results, instance)
    check_privileges(results)
    check_network(results, resolver)
    violations = sorted(name for name, result in results.items() if not result["ok"])
    print(json.dumps({"probe": "flowerly-provider-remote", "instance": instance,
                      "checks": results, "violations": violations}, indent=2, sort_keys=True))
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

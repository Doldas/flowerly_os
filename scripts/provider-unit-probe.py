#!/usr/bin/env python3
"""Confinement probe for the remote provider unit (ADR-0022, #263).

Runs INSIDE `flowerly-provider-remote@probe.service` on a disposable Arch
Linux/KVM guest, with the unit's ExecStart replaced by this script through a
drop-in, so every confinement setting of the real template applies. See
web-docs/docs/remote-provider-unit-test-plan.md.

    provider-unit-probe.py [--targets FILE] [--control-out FILE | --control FILE]

Each check attempts one thing the provider must not be able to do and
classifies what happened:

- blocked_by_os         EPERM/EACCES (EAFNOSUPPORT for a socket family,
                        EROFS for a write): the kernel refused it
- hidden_by_os          ENOENT for a path the control run proved exists,
                        or for /proc and /dev nodes the unit hides
- connection_refused    ECONNREFUSED: nothing listened, nothing was blocked
- timed_out             no answer: proves nothing
- allowed               it worked
- inconclusive          anything else (no route, invalid address, ENOENT
                        without a control run proving the path exists)
- refused_by_application is reserved for providerd's own refusals, which
                        the guest runner records; the probe never uses it

A refused connection, a timeout or a missing route is never counted as a
working boundary. Network and file checks are only conclusive against a
control run: the guest runner first runs this probe as root OUTSIDE the
unit with --control-out, which must see every listener answer and every
path exist; the in-unit run reads that file with --control.

It never reads secret contents: a file check opens and closes, a
credential check lists names and sizes. Output is one JSON object. Exit 1
if any required check is violated or inconclusive.

Offline, run outside any sandbox, it must report violations: that is how
its own detection is tested (scripts/test-remote-provider-operator.py and
scripts/test-remote-provider-kvm-harness.py).
"""
import argparse
import errno
import grp
import json
import os
import pwd
import signal
import socket
import sys

# SIGSYS: the unit's SystemCallFilter killed the (forked) caller.
BLOCKED_ERRNOS = {"EPERM", "EACCES", "SIGSYS"}
FAMILY_BLOCKED_ERRNOS = {"EAFNOSUPPORT", "EPERM", "EACCES", "EPROTONOSUPPORT"}
WRITE_BLOCKED_ERRNOS = {"EPERM", "EACCES", "EROFS"}
ZERO_CAPS = "0000000000000000"
NAMESPACES = ["CLONE_NEWUSER", "CLONE_NEWNS", "CLONE_NEWNET", "CLONE_NEWIPC",
              "CLONE_NEWUTS", "CLONE_NEWPID", "CLONE_NEWCGROUP"]

# Used when no --targets file is given (offline self-test). The guest
# runner passes testbed/remote-provider-kvm/probe-targets.json.
DEFAULT_TARGETS = {
    "instance": "probe",
    "provider_user": "flowerly-provider",
    "client_group": "flowerly-provider-clients",
    "read_denied": [
        "/var/lib/flowerly/state.db", "/var/lib/flowerly", "/run/flowerly", "/var/lib/flowerly-evolution",
        "/etc/credstore", "/etc/credstore.encrypted", "/etc/credstore.encrypted/{instance}.key",
        "/etc/shadow", "/etc/gshadow", "/root", "/home",
        "/var/lib/flowerly-provider-operator", "/var/log/flowerly-provider-acceptance"],
    "write_denied": ["/etc/flowerly-probe", "/usr/local/flowerly-probe", "/var/lib/flowerly-probe",
                     "/run/flowerly-probe", "/home/flowerly-probe"],
    "connect_blocked": [
        {"name": "loopback_v4", "address": "127.0.0.1", "port": 22},
        {"name": "loopback_v6", "address": "::1", "port": 22},
        {"name": "metadata_v4", "address": "169.254.169.254", "port": 80}],
    "connect_not_blocked": [],
    "dns_stub": None,
}


def errname(err):
    return errno.errorcode.get(err.errno, str(err.errno)) if err.errno else type(err).__name__


def attempt(action):
    """'allowed' or the errno name."""
    try:
        action()
        return "allowed"
    except socket.timeout:
        return "ETIMEDOUT"
    except OSError as err:
        return errname(err)


def classify_connect(observed):
    if observed == "allowed":
        return "allowed"
    if observed in BLOCKED_ERRNOS:
        return "blocked_by_os"
    if observed == "ECONNREFUSED":
        return "connection_refused"
    if observed == "ETIMEDOUT":
        return "timed_out"
    return "inconclusive"


def classify_denial(observed, blocked, enoent_means_hidden):
    if observed == "allowed":
        return "allowed"
    if observed in blocked:
        return "blocked_by_os"
    if observed == "ENOENT" and enoent_means_hidden:
        return "hidden_by_os"
    return "inconclusive"


def add(results, name, expected, observed, classification, required=True, control=None, controlled=False):
    """`controlled`: this check is only conclusive if the control run
    reached the target; without that, any denial is inconclusive."""
    accepted = expected if isinstance(expected, list) else [expected]
    ok = classification in accepted
    if controlled and control != "allowed" and classification != "allowed":
        classification, ok = "inconclusive", False
    entry = {"expected": expected, "observed": observed, "classification": classification,
             "required": required, "ok": ok}
    if controlled:
        entry["control"] = control
    results[name] = entry


def must_fail(name, action, results, accept=None):
    """Fixed-input classification hook for the offline operator test."""
    observed = attempt(action)
    if accept is None:
        classification = "allowed" if observed == "allowed" else "blocked_by_os"
    else:
        classification = "blocked_by_os" if observed in accept else classify_connect(observed)
    add(results, name, "blocked_by_os", observed, classification)


BLOCKED = BLOCKED_ERRNOS


def open_and_discard(path, mode="rb"):
    def action():
        if "r" in mode and os.path.isdir(path):
            os.listdir(path)
            return
        with open(path, mode):
            pass
    return action


def status_fields():
    fields = {}
    with open("/proc/self/status") as status:
        for line in status:
            key, _, value = line.partition(":")
            fields[key] = value.strip()
    return fields


def connect(target, timeout=3.0):
    def action():
        address = target["address"]
        if ":" in address:
            scope = socket.if_nametoindex(target["interface"]) if target.get("interface") else 0
            family, sockaddr = socket.AF_INET6, (address, target["port"], 0, scope)
        else:
            family, sockaddr = socket.AF_INET, (address, target["port"])
        with socket.socket(family, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout)
            sock.connect(sockaddr)
    return action


def in_child(action):
    """Run `action` in a forked child so a success (a new user namespace,
    say) cannot change this process. Returns 'allowed', an errno name, or
    'SIGSYS' when the seccomp filter killed the child for the syscall."""
    read_end, write_end = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_end)
        os.write(write_end, attempt(action).encode()[:64])
        os._exit(0)
    os.close(write_end)
    with os.fdopen(read_end) as pipe:
        observed = pipe.read()
    _, status = os.waitpid(pid, 0)
    if not observed:
        if os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGSYS:
            return "SIGSYS"
        return "no_result"
    return observed


def control_of(control, name):
    return ((control or {}).get(name) or {}).get("observed")


def check_files(results, targets, control):
    for path in targets["read_denied"]:
        path = path.format(instance=targets["instance"])
        name = f"read {path}"
        observed = attempt(open_and_discard(path))
        exists = control_of(control, name) == "allowed"
        add(results, name, ["blocked_by_os", "hidden_by_os"], observed,
            classify_denial(observed, BLOCKED_ERRNOS, exists),
            control=control_of(control, name), controlled=control is not None)
        if control is not None:
            results[name]["dac_open"] = bool(((control.get(name) or {}).get("dac_open")))
    for path in targets["write_denied"]:
        observed = attempt(open_and_discard(path, "xb"))
        if observed == "allowed":
            os.unlink(path)
        add(results, f"write {path}", "blocked_by_os", observed,
            classify_denial(observed, WRITE_BLOCKED_ERRNOS, False))
    runtime = os.environ.get("RUNTIME_DIRECTORY")
    if runtime:
        path = os.path.join(runtime.split(":")[0], "probe-write-check")
        observed = attempt(open_and_discard(path, "xb"))
        if observed == "allowed":
            os.unlink(path)
        add(results, "write own RuntimeDirectory (informational)", "allowed", observed,
            classify_connect(observed), required=False)


def check_credentials(results, instance):
    directory = os.environ.get("CREDENTIALS_DIRECTORY")
    names = sorted(os.listdir(directory)) if directory else []
    exact = names == [f"{instance}.key"]
    add(results, "credentials directory holds only this instance's key", "exact", names,
        "exact" if exact else "unexpected")
    if exact:
        # Size only: the contents are never read.
        size = os.stat(os.path.join(directory, names[0])).st_size
        add(results, "exposed credential is non-empty (size only)", "nonempty", size,
            "nonempty" if size > 0 else "empty")


def check_privileges(results, targets):
    fields = status_fields()
    for key in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"):
        add(results, f"status {key}", ZERO_CAPS, fields.get(key), fields.get(key))
    add(results, "status NoNewPrivs", "1", fields.get("NoNewPrivs"), fields.get("NoNewPrivs"))
    add(results, "status Seccomp", "2", fields.get("Seccomp"), fields.get("Seccomp"))
    uids = fields.get("Uid", "").split()
    gids = fields.get("Gid", "").split()
    groups = fields.get("Groups", "").split()
    try:
        want_uid = str(pwd.getpwnam(targets["provider_user"]).pw_uid)
    except KeyError:
        want_uid = None
    try:
        want_gid = str(grp.getgrnam(targets["client_group"]).gr_gid)
    except KeyError:
        want_gid = None
    add(results, "uid real/effective/saved/fs", "match", uids,
        "match" if want_uid and uids == [want_uid] * 4 else "mismatch")
    add(results, "gid real/effective/saved/fs", "match", gids,
        "match" if want_gid and gids == [want_gid] * 4 else "mismatch")
    add(results, "not root", "nonroot", uids + gids,
        "nonroot" if uids and gids and "0" not in uids + gids else "root_or_unknown")
    primary = gids[1] if len(gids) > 1 else None
    add(results, "no supplementary group", "none", groups,
        "none" if primary and all(g == primary for g in groups) else "supplementary")
    observed = in_child(lambda: os.setuid(0))
    add(results, "setuid(0)", "blocked_by_os", observed, classify_denial(observed, BLOCKED_ERRNOS, False))
    for name, path, mode in [("read /proc/1/environ", "/proc/1/environ", "rb"),
                             ("list /proc/1/fd", "/proc/1/fd", "rb"),
                             ("write /proc/sys/kernel/hostname", "/proc/sys/kernel/hostname", "wb"),
                             ("open /dev/kvm", "/dev/kvm", "rb"), ("open /dev/mem", "/dev/mem", "rb")]:
        observed = attempt(open_and_discard(path, mode))
        # ProtectProc=invisible and PrivateDevices hide these: ENOENT counts.
        add(results, name, ["blocked_by_os", "hidden_by_os"], observed,
            classify_denial(observed, WRITE_BLOCKED_ERRNOS, True))
    for flag in NAMESPACES:
        if not hasattr(os, "unshare") or not hasattr(os, flag):
            add(results, f"unshare {flag}", "blocked_by_os", "unsupported_by_python", "inconclusive")
            continue
        observed = in_child(lambda flag=flag: os.unshare(getattr(os, flag)))
        add(results, f"unshare {flag}", "blocked_by_os", observed,
            classify_denial(observed, BLOCKED_ERRNOS, False))


def check_network(results, targets, control):
    for target in targets["connect_blocked"]:
        name = f"connect {target['name']}"
        observed = attempt(connect(target))
        add(results, name, "blocked_by_os", observed, classify_connect(observed),
            control=control_of(control, name), controlled=control is not None)
    for target in targets["connect_not_blocked"]:
        name = f"connect {target['name']} (application policy only)"
        observed = attempt(connect(target))
        add(results, name, "allowed", observed, classify_connect(observed), required=False,
            control=control_of(control, name), controlled=control is not None)
    for name, family, kind, proto in [
            ("AF_PACKET socket", socket.AF_PACKET, socket.SOCK_RAW, 0),
            ("AF_NETLINK socket", socket.AF_NETLINK, socket.SOCK_RAW, 0),
            ("raw ICMP socket", socket.AF_INET, socket.SOCK_RAW, socket.IPPROTO_ICMP),
            ("AF_VSOCK socket", getattr(socket, "AF_VSOCK", 40), socket.SOCK_STREAM, 0),
            ("AF_ALG socket", getattr(socket, "AF_ALG", 38), socket.SOCK_SEQPACKET, 0),
            ("AF_BLUETOOTH socket", getattr(socket, "AF_BLUETOOTH", 31), socket.SOCK_STREAM, 0)]:
        observed = attempt(lambda f=family, k=kind, p=proto: socket.socket(f, k, p).close())
        add(results, name, "blocked_by_os", observed, classify_denial(observed, FAMILY_BLOCKED_ERRNOS, False))
    stub = targets.get("dns_stub")
    if stub:
        # The loopback DNS stub the operator drop-in allows must stay usable.
        observed = attempt(connect({"address": stub, "port": 53}))
        add(results, f"connect DNS stub {stub}:53", ["allowed", "connection_refused"], observed,
            classify_connect(observed))


def dac_open(path):
    """Whether ordinary permissions alone would let any user read `path`:
    every ancestor searchable by others, the path itself readable by
    others. A denial of such a path can only come from the unit."""
    parts = os.path.abspath(path).split("/")[1:]
    for depth in range(1, len(parts)):
        if not os.stat("/" + "/".join(parts[:depth])).st_mode & 0o001:
            return False
    return bool(os.stat(path).st_mode & 0o004)


def control_run(targets):
    """As root outside the unit: every listener answers, every path exists.
    Records which paths ordinary permissions leave open (`dac_open`)."""
    results = {}
    for path in targets["read_denied"]:
        path = path.format(instance=targets["instance"])
        observed = "allowed" if os.path.lexists(path) else "ENOENT"
        add(results, f"read {path}", "allowed", observed, observed)
        results[f"read {path}"]["dac_open"] = observed == "allowed" and dac_open(path)
    for target in targets["connect_blocked"]:
        observed = attempt(connect(target))
        add(results, f"connect {target['name']}", "allowed", observed, classify_connect(observed))
    for target in targets["connect_not_blocked"]:
        observed = attempt(connect(target))
        add(results, f"connect {target['name']} (application policy only)", "allowed", observed,
            classify_connect(observed))
    return results


def main(argv):
    parser = argparse.ArgumentParser(description="remote provider unit confinement probe")
    parser.add_argument("--targets")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--control-out")
    mode.add_argument("--control")
    args = parser.parse_args(argv[1:])
    targets = dict(DEFAULT_TARGETS)
    if args.targets:
        with open(args.targets) as handle:
            targets.update(json.load(handle))
    targets["instance"] = os.environ.get("FLOWERLY_PROBE_INSTANCE", targets["instance"])
    if args.control_out:
        results = control_run(targets)
        unreachable = sorted(name for name, entry in results.items() if not entry["ok"])
        with open(args.control_out, "x") as out:
            json.dump({"mode": "control", "checks": results, "unreachable": unreachable},
                      out, indent=2, sort_keys=True)
        print(json.dumps({"mode": "control", "unreachable": unreachable}))
        return 1 if unreachable else 0
    control = None
    if args.control:
        with open(args.control) as handle:
            control = json.load(handle)["checks"]
    results = {}
    check_files(results, targets, control)
    check_credentials(results, targets["instance"])
    check_privileges(results, targets)
    check_network(results, targets, control)
    required = {name: entry for name, entry in results.items() if entry["required"]}
    inconclusive = sorted(name for name, entry in required.items() if entry["classification"] == "inconclusive")
    violations = sorted(name for name, entry in required.items()
                        if not entry["ok"] and name not in inconclusive)
    print(json.dumps({"probe": "flowerly-provider-remote", "mode": "unit", "instance": targets["instance"],
                      "controlled": control is not None, "checks": results,
                      "violations": violations, "inconclusive": inconclusive}, indent=2, sort_keys=True))
    return 1 if violations or inconclusive else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

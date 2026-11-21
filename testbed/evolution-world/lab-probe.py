#!/usr/bin/env python3
"""Evolution Lab guest probe (ADR-0024 D2), A/B test profile only.

`evolutiond lab run` boots a candidate disk under QEMU with no network and no
host sharing, so this service reads what booted and sends it back on the
serial console. It reports and never judges: the host compares the report
with the plan and decides. Anything the probe cannot determine is null.

The report (flowerly-lab-report/v1) is a base64 JSON document split into
console lines:

    FLOWERLY_LAB_REPORT part=i/n <base64>
    FLOWERLY_LAB_REPORT_DONE parts=n sha256=<hex of the JSON>

The unit then powers the guest off, also when the probe fails, so QEMU (run
with -no-reboot) exits on its own; the host records that exit as the shutdown
check.

Facts come from the booted system, not from what the baked identity claims:
the slot and PARTUUID from the partition mounted at /, the kernel image from
that slot's ESP directory, packages from the pacman database. The Admin round
trip runs as the flowerly-lab account through agenticctl and flowerlyd, never
as root and never by writing the database itself; the Generation it creates
and rejects lives in this guest's own test database.

`lab-probe.py --offline ROOT` prints the same file facts for an unbooted
root tree, so a candidate build can write its plan with the probe's hashing.
"""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import termios
import time

SCHEMA = "flowerly-lab-report/v1"
CONSOLE = "/dev/console"
IDENTITY = "usr/lib/flowerly/generation-root.json"
PACMAN_LOCAL = "var/lib/pacman/local"
BINARIES = {
    "agenticctl": "usr/bin/agenticctl",
    "evolutiond": "usr/bin/evolutiond",
    "flowerlyd": "usr/bin/flowerlyd",
    "flowerly-slot-broker": "usr/lib/flowerly/flowerly-slot-broker",
}
SLOTS = {"root-a": "a", "root-b": "b"}
LAB_USER = "flowerly-lab"
AGENTICCTL = ["/usr/bin/agenticctl", "--runtime-dir", "/run/flowerly"]
HEALTH_UNIT = "flowerly-generation-health.service"
ROUNDTRIP_REASON = "evolution lab probe: Admin round trip"
TIMESTAMPS = ("Firmware", "Loader", "InitRD", "Userspace", "Finish")
CHUNK = 480
MAX_IDENTITY = 4096
MAX_DESC = 1024 * 1024
MAX_PACKAGES = 65536
MAX_FAILED_UNITS = 64
TIMEOUT = 60
DEVNAME = re.compile(r"[A-Za-z0-9_-]{1,64}")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
BUILD_ID = re.compile(r"[A-Za-z0-9_.-]{1,64}")
GENERATION = re.compile(r"[A-Za-z0-9_-]{1,128}")
UNIT = re.compile(r"[A-Za-z0-9:_.@\\-]{1,256}")
PACKAGE_FIELD = re.compile(r"[A-Za-z0-9@._+:~-]{1,256}")


def sha256_file(path):
    """Hex digest of a regular file that is not a symlink, else None."""
    path = Path(path)
    try:
        if path.is_symlink() or not path.is_file():
            return None
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for block in iter(lambda: handle.read(1 << 20), b""):
                digest.update(block)
        return digest.hexdigest()
    except OSError:
        return None


def identity(root):
    """(SHA-256, build_id) of the baked identity document."""
    path = Path(root) / IDENTITY
    try:
        if path.is_symlink() or not path.is_file():
            return None, None
        raw = path.read_bytes()
    except OSError:
        return None, None
    if len(raw) > MAX_IDENTITY:
        return None, None
    try:
        build_id = json.loads(raw).get("build_id")
    except (ValueError, AttributeError):
        build_id = None
    if not isinstance(build_id, str) or not BUILD_ID.fullmatch(build_id):
        build_id = None
    return hashlib.sha256(raw).hexdigest(), build_id


def _desc_fields(text):
    """%NAME% and %VERSION% of one pacman desc file."""
    fields, key = {}, None
    for line in text.splitlines():
        if line.startswith("%") and line.endswith("%"):
            key = line
        elif key in ("%NAME%", "%VERSION%") and key not in fields and line:
            fields[key] = line
    name, version = fields.get("%NAME%"), fields.get("%VERSION%")
    if not name or not version:
        raise ValueError("desc without %NAME% and %VERSION%")
    if not PACKAGE_FIELD.fullmatch(name) or not PACKAGE_FIELD.fullmatch(version):
        raise ValueError("unexpected package name or version")
    return name, version


def packages(root):
    """(SHA-256 of the sorted `name version` lines, count), or (None, None).

    One unreadable entry makes the whole set unknown: a partial hash would
    look like a different package set.
    """
    local = Path(root) / PACMAN_LOCAL
    try:
        if local.is_symlink() or not local.is_dir():
            return None, None
        lines = []
        for entry in sorted(local.iterdir()):
            if entry.name == "ALPM_DB_VERSION":
                continue
            desc = entry / "desc"
            if (
                desc.is_symlink()
                or not desc.is_file()
                or desc.stat().st_size > MAX_DESC
            ):
                return None, None
            name, version = _desc_fields(desc.read_text())
            lines.append("%s %s\n" % (name, version))
            if len(lines) > MAX_PACKAGES:
                return None, None
    except (OSError, UnicodeError, ValueError):
        return None, None
    if not lines:
        return None, None
    body = "".join(sorted(lines)).encode()
    return hashlib.sha256(body).hexdigest(), len(lines)


def binaries(root):
    """Flowerly binary name -> SHA-256, for every one that is present."""
    found = {}
    for name, relative in BINARIES.items():
        digest = sha256_file(Path(root) / relative)
        if digest is not None:
            found[name] = digest
    return found


def file_facts(root):
    identity_sha256, build_id = identity(root)
    packages_sha256, package_count = packages(root)
    return {
        "identity_sha256": identity_sha256,
        "build_id": build_id,
        "packages_sha256": packages_sha256,
        "package_count": package_count,
        "binaries": binaries(root),
    }


def _uevent(path):
    fields = {}
    for line in Path(path).read_text().splitlines():
        key, sep, value = line.partition("=")
        if sep:
            fields[key] = value
    return fields


def booted_root(device, sys_root="/sys", dev_root="/dev"):
    """(slot, PARTUUID) of the partition holding ``device`` (st_dev of /).

    The slot is the GPT partition name; the PARTUUID is the udev by-partuuid
    link that resolves to the same block device.
    """
    node = "%s/dev/block/%d:%d" % (sys_root, os.major(device), os.minor(device))
    try:
        fields = _uevent(node + "/uevent")
    except (OSError, UnicodeError):
        return None, None
    slot = fields.get("PARTNAME")
    slot = slot if slot in SLOTS else None
    devname = fields.get("DEVNAME")
    if not devname or not DEVNAME.fullmatch(devname):
        return slot, None
    target = os.path.realpath(os.path.join(dev_root, devname))
    links = Path(dev_root) / "disk" / "by-partuuid"
    try:
        matches = [
            link.name.lower()
            for link in sorted(links.iterdir())
            if os.path.realpath(link) == target
        ]
    except OSError:
        return slot, None
    if len(matches) != 1 or not UUID.fullmatch(matches[0]):
        return slot, None
    return slot, matches[0]


def kernel(slot, efi_root="/efi", release=None):
    """(release, SHA-256 of the slot's ESP kernel image)."""
    release = release if release is not None else os.uname().release
    letter = SLOTS.get(slot)
    image = None
    if letter is not None:
        image = sha256_file(Path(efi_root) / "flowerly" / letter / "vmlinuz")
    return release, image


def run(argv, timeout=TIMEOUT):
    """(exit status, stdout, stderr); status None when it could not run.

    A command that does not answer within ``timeout`` counts as failed.
    """
    try:
        result = subprocess.run(
            argv, capture_output=True, text=True, timeout=timeout, check=False
        )
    except subprocess.TimeoutExpired:
        return 124, "", "timed out"
    except OSError as error:
        return None, "", str(error)
    return result.returncode, result.stdout, result.stderr


def _show(runner, unit=None, properties=()):
    argv = ["systemctl", "show"]
    for name in properties:
        argv += ["-p", name]
    if unit is not None:
        argv.append(unit)
    code, out, _ = runner(argv)
    if code != 0:
        return None
    fields = {}
    for line in out.splitlines():
        key, sep, value = line.partition("=")
        if sep:
            fields[key] = value
    return fields


def boot_phases(runner, now_us=None):
    """systemd-analyze's boot phases in milliseconds.

    The probe runs before boot finishes, so userspace ends now unless
    FinishTimestamp is already set. Phases without a timestamp are omitted.
    """
    fields = _show(runner, properties=[t + "TimestampMonotonic" for t in TIMESTAMPS])
    if fields is None:
        return {}
    try:
        stamp = {t: int(fields.get(t + "TimestampMonotonic", "0")) for t in TIMESTAMPS}
    except ValueError:
        return {}
    if now_us is None:
        now_us = time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000
    phases = {}
    if stamp["Firmware"] and stamp["Loader"]:
        phases["firmware"] = stamp["Firmware"] - stamp["Loader"]
    if stamp["Loader"]:
        phases["loader"] = stamp["Loader"]
    if stamp["Userspace"]:
        if stamp["InitRD"]:
            phases["kernel"] = stamp["InitRD"]
            phases["initrd"] = stamp["Userspace"] - stamp["InitRD"]
        else:
            phases["kernel"] = stamp["Userspace"]
        phases["userspace"] = (stamp["Finish"] or now_us) - stamp["Userspace"]
    return {k: round(v / 1000, 3) for k, v in phases.items() if v >= 0}


def health_gate(runner):
    """`passed`, `failed` or `absent`, from the health unit's last run.

    The unit is a oneshot without RemainAfterExit: after success it is
    inactive with Result=success and a started, zero-status main process.
    """
    fields = _show(
        runner,
        HEALTH_UNIT,
        [
            "LoadState",
            "ActiveState",
            "Result",
            "ExecMainStartTimestampMonotonic",
            "ExecMainStatus",
        ],
    )
    if fields is None:
        return None
    if fields.get("LoadState") == "not-found":
        return "absent"
    passed = (
        fields.get("LoadState") == "loaded"
        and fields.get("ActiveState") in ("inactive", "active")
        and fields.get("Result") == "success"
        and fields.get("ExecMainStartTimestampMonotonic", "0") not in ("", "0")
        and fields.get("ExecMainStatus") == "0"
    )
    return "passed" if passed else "failed"


def failed_units(runner):
    code, out, _ = runner(
        [
            "systemctl",
            "list-units",
            "--state=failed",
            "--plain",
            "--no-legend",
            "--no-pager",
        ]
    )
    if code != 0:
        # Unknown is not "none failed".
        return ["(unknown)"]
    units = []
    for line in out.splitlines():
        words = line.split()
        if words:
            units.append(words[0] if UNIT.fullmatch(words[0]) else "(unprintable unit)")
    return sorted(units)[:MAX_FAILED_UNITS]


def as_lab(argv):
    return ["runuser", "-u", LAB_USER, "--"] + argv


def _json(out):
    try:
        return json.loads(out)
    except ValueError:
        return None


def admin_roundtrip(runner):
    """Create a Generation, reject it as Admin, and read the rejection back."""
    code, out, _ = runner(as_lab(AGENTICCTL + ["generation", "create"]))
    created = _json(out) if code == 0 else None
    if (
        not isinstance(created, dict)
        or created.get("status") != "CANDIDATE"
        or not isinstance(created.get("id"), str)
        or not GENERATION.fullmatch(created["id"])
    ):
        return False
    generation = created["id"]
    code, out, _ = runner(
        as_lab(
            AGENTICCTL
            + ["generation", "reject", generation, "--reason", ROUNDTRIP_REASON]
        )
    )
    if code != 0 or _json(out) != {"id": generation, "status": "REJECTED"}:
        return False
    code, out, _ = runner(as_lab(AGENTICCTL + ["generation", "inspect", generation]))
    seen = _json(out) if code == 0 else None
    return (
        isinstance(seen, dict)
        and seen.get("id") == generation
        and seen.get("status") == "REJECTED"
    )


def _succeeded(code):
    return None if code is None else code == 0


def observations(runner):
    flowerlyd = runner(["systemctl", "is-active", "--quiet", "flowerlyd.service"])[0]
    status = runner(as_lab(AGENTICCTL + ["status"]))[0]
    self_test = runner(as_lab(["/usr/bin/evolutiond", "--self-test"]))[0]
    return {
        "health_gate": health_gate(runner),
        "flowerlyd_active": _succeeded(flowerlyd),
        "status_ok": _succeeded(status),
        "admin_roundtrip": admin_roundtrip(runner),
        "evolutiond_self_test": _succeeded(self_test),
    }


def probe(
    runner,
    root="/",
    efi_root="/efi",
    sys_root="/sys",
    dev_root="/dev",
    device=None,
    release=None,
    now_us=None,
):
    """The report for this boot."""
    if device is None:
        device = os.stat(root).st_dev
    slot, partuuid = booted_root(device, sys_root, dev_root)
    kernel_release, vmlinuz_sha256 = kernel(slot, efi_root, release)
    facts = {"slot": slot, "root_partuuid": partuuid}
    facts.update(file_facts(root))
    facts.update(
        {
            "kernel_release": kernel_release,
            "vmlinuz_sha256": vmlinuz_sha256,
            "boot_phases_ms": boot_phases(runner, now_us),
            "failed_units": failed_units(runner),
        }
    )
    return {"schema": SCHEMA, "facts": facts, "observations": observations(runner)}


def report_lines(report):
    """Console lines carrying ``report``; the DONE line names its digest."""
    body = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    encoded = base64.b64encode(body).decode()
    chunks = [encoded[i : i + CHUNK] for i in range(0, len(encoded), CHUNK)]
    lines = [
        "FLOWERLY_LAB_REPORT part=%d/%d %s" % (i + 1, len(chunks), chunk)
        for i, chunk in enumerate(chunks)
    ]
    lines.append(
        "FLOWERLY_LAB_REPORT_DONE parts=%d sha256=%s"
        % (len(chunks), hashlib.sha256(body).hexdigest())
    )
    return lines


def console_emitter(
    path,
    opener=os.open,
    write=os.write,
    drain=termios.tcdrain,
    echo=lambda line: print(line, flush=True),
):
    """An emit that returns only once the console has sent the line.

    The same console discipline as the G5 drill: lines journald still queues
    at power-off are lost, so each line is written and drained here and only
    echoed to the journal. The unit runs before the serial getty, whose start
    flushes the TTY.
    """
    fd = opener(path, os.O_WRONLY | os.O_NOCTTY | os.O_CLOEXEC)

    def emit(line):
        echo(line)
        data = memoryview((line + "\n").encode())
        while data:
            data = data[write(fd, data) :]
        drain(fd)

    return emit


def main(argv):
    if len(argv) == 2 and argv[0] == "--offline":
        json.dump(file_facts(argv[1]), sys.stdout, sort_keys=True, indent=2)
        sys.stdout.write("\n")
        return 0
    if argv:
        print("usage: lab-probe.py [--offline ROOT]", file=sys.stderr)
        return 2
    emit = console_emitter(CONSOLE)
    for line in report_lines(probe(run)):
        emit(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

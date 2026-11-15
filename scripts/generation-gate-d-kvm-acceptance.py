#!/usr/bin/env python3
"""Opt-in REAL OVMF/KVM Gate D acceptance (ADR-0023 G5), test profile only.

Every Admin decision runs inside the guest through the real agenticctl ->
flowerlyd -> slot broker path (testbed/generation-ab/g5-drill.py). The host
only writes the next instruction to the ESP while the guest is off, edits what
an operator or a fault would edit (an injected unhealthy option, the boot menu
choice), stages the schema-bump image, and reads the drill's report and the
boot identity from the serial console.

Main chain: A registers A and B and activates B with an injected unhealthy
option; B fails its boot count and the loader falls back to A, which records
FELL_BACK and refuses to re-activate B without the Admin's acknowledgement
(Q4); the acknowledged activation of B is confirmed; B activates A, A confirms
and rolls back to B; B confirms and rolls back to A unhealthy again; that
rollback falls back to B, which retries it (a rollback needs no
acknowledgement) and A confirms. Every expected refusal must name its reason.
Schema chain: B2 (flowerlyd with the test-only g5-schema-drill migration) is
staged in slot B; its activation is irreversible and needs the explicit
acknowledgement and a backup; the older A then refuses to read the database
and B2 refuses the rollback; the operator boots the demoted A by hand and
restores the backup. A copy of the disk with an exhausted target and no other
default entry shows what systemd-boot does then. Not release acceptance.
"""

import argparse
import base64
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
SCENARIOS = (
    "register-tested",
    "activate-b-confirmed",
    "activate-a-confirmed",
    "rollback-b-confirmed",
    "unhealthy-activation-fell-back",
    "unhealthy-rollback-fell-back",
    "fell-back-needs-acknowledgement",
    "acknowledged-reactivation-confirmed",
    "irreversible-needs-acknowledgement",
    "irreversible-activation-confirmed",
    "downgrade-refused-across-schema",
    "older-root-cannot-read-the-database",
    "restore-drill",
    "user-data-intact",
    "history-readable-every-step",
    "exhausted-target-observed",
)
TOKEN = "I_APPROVE_GATE_D_KVM_EXECUTION"
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")
PART = re.compile(r"FLOWERLY_G5_DRILL step=(\d+) part=(\d+)/(\d+) ([A-Za-z0-9+/=]+)")
DONE = re.compile(
    r"FLOWERLY_G5_DRILL_DONE step=(\d+) parts=(\d+) sha256=([0-9a-f]{64}) ok=(true|false)"
)
MARKER = re.compile(r"FLOWERLY_GENERATION_USER_MARKER_SHA256=([0-9a-f]{64})")
SUPERVISOR_FAILED = "Failed to start Flowerly OS Unified Supervisor"
SGR = re.compile(r"\x1b\[[0-9;:]*m")
INSTRUCTION = "/flowerly/g5-drill.json"
ESP_MANIFEST = "/flowerly/ab-disk.manifest.json"
FAIL_HEALTH = "flowerly.test.fail_health=1"
# Seconds the exhausted-target copy is watched before its outcome is recorded.
OBSERVE_SECONDS = 120


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    if spec is None or spec.loader is None:
        raise ValueError("missing helper: " + file)
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


DRILL = load("g5_drill", "testbed/generation-ab/g5-drill.py")


def sha_file(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as source:
        while part := source.read(1048576):
            value.update(part)
    return value.hexdigest()


# ---- pure parts (offline-tested) -------------------------------------------


def parse_drill(text, step):
    """The drill's report for ``step`` from a serial log, or None if absent.

    Console noise may surround a line; a part that differs from an earlier
    copy, a missing part or a digest mismatch is refused, never repaired.
    """
    done = [m for m in DONE.finditer(text) if int(m.group(1)) == step]
    if not done:
        return None
    if len(done) != 1:
        raise ValueError(f"drill step {step} reported more than once")
    count, digest, ok = int(done[0].group(2)), done[0].group(3), done[0].group(4)
    parts = {}
    for m in PART.finditer(text):
        if int(m.group(1)) != step:
            continue
        index, total = int(m.group(2)), int(m.group(3))
        if total != count or not 1 <= index <= count:
            raise ValueError(f"drill step {step} part {index}/{total} is inconsistent")
        if parts.setdefault(index, m.group(4)) != m.group(4):
            raise ValueError(f"drill step {step} part {index} differs between copies")
    if sorted(parts) != list(range(1, count + 1)):
        raise ValueError(f"drill step {step} report is missing parts")
    body = base64.b64decode(
        "".join(parts[i] for i in range(1, count + 1)), validate=True
    )
    if hashlib.sha256(body).hexdigest() != digest:
        raise ValueError(f"drill step {step} report does not match its digest")
    report = json.loads(body)
    if report.get("step") != step or report.get("ok") != (ok == "true"):
        raise ValueError(f"drill step {step} report disagrees with its DONE line")
    return report


def supervisor_failed(text):
    """systemd reported that flowerlyd failed; its status lines are coloured."""
    return SUPERVISOR_FAILED in SGR.sub("", text)


def instruction(step, slot, actions, reboot):
    """A drill instruction, checked by the guest's own validator."""
    doc = {
        "schema": DRILL.SCHEMA,
        "step": step,
        "slot": slot,
        "actions": actions,
        "reboot": reboot,
    }
    DRILL.load_instruction(json.dumps(doc))
    return doc


def registration(manifest, slot, label):
    """The register action for a staged slot, from the disk manifest."""
    part = next(p for p in manifest["partitions"] if p["name"] == slot)
    boot, ident = part.get("boot"), part.get("identity")
    if not isinstance(boot, dict) or not isinstance(ident, dict):
        raise ValueError(f"{slot} has no staged boot payload and identity")
    if ident.get("slot") != slot or ident.get("root_partuuid") != part["partuuid"]:
        raise ValueError(f"{slot} identity names another slot")
    identity = {
        "source_commit": ident["source_commit"],
        "rootfs_sha256": part["sha256"],
        "kernel_sha256": boot["kernel_sha256"],
        "initramfs_sha256": boot["initramfs_sha256"],
        "input_manifest_sha256": ident["input_manifest_sha256"],
        "build_id": ident["build_id"],
    }
    evidence = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
    return {
        "op": "register",
        "label": label,
        "identity": identity,
        "evidence_sha256": evidence,
    }


def with_option(entry_text, option):
    """A boot entry with one more kernel option on its options line."""
    lines = entry_text.splitlines()
    found = [i for i, line in enumerate(lines) if line.startswith("options ")]
    if len(found) != 1:
        raise ValueError("boot entry needs exactly one options line")
    if option in lines[found[0]].split():
        raise ValueError("boot entry already has " + option)
    lines[found[0]] += " " + option
    return "\n".join(lines) + "\n"


def with_default(loader_text, pattern):
    """loader.conf with its default line replaced (the operator's choice)."""
    lines = loader_text.splitlines()
    found = [i for i, line in enumerate(lines) if line.startswith("default ")]
    if len(found) != 1:
        raise ValueError("loader.conf needs exactly one default line")
    lines[found[0]] = "default " + pattern
    return "\n".join(lines) + "\n"


def entry(report, call, index=-1):
    calls = [e for e in report["entries"] if e.get("call") == call]
    if not calls:
        raise ValueError(f"drill step {report['step']} has no {call} call")
    return calls[index]


def registered(report, label):
    for item in report["entries"]:
        if item.get("call") == "registered" and item.get("label") == label:
            return item["generation"]
    raise ValueError(f"drill step {report['step']} did not register {label}")


def activations(report):
    """The activation history the drill read at the end of its step."""
    history = entry(report, "history")
    if history["code"] != 0 or not isinstance(history["reply"], dict):
        raise ValueError(f"history unreadable after step {report['step']}")
    return history["reply"]["activations"]


def last_activation(report, generation):
    rows = [a for a in activations(report) if a["generation"] == generation]
    if not rows:
        raise ValueError(f"no activation of {generation} after step {report['step']}")
    return max(rows, key=lambda a: a["id"])


def is_confirmed(report, generation, kind):
    """Whether the newest activation of ``generation`` is a CONFIRMED ``kind``."""
    row = last_activation(report, generation)
    return row["kind"] == kind and row["state"] == "CONFIRMED"


def history_readable(report):
    return all(
        entry(report, name)["code"] == 0
        and isinstance(entry(report, name)["reply"], dict)
        for name in ("list", "history")
    )


def refused(report, call, index=-1, because=None):
    """The text of an expected refusal (non-zero exit with a reason), which
    must name ``because`` when given: a refusal for another reason proves
    nothing about the guard under test."""
    item = entry(report, call, index)
    if item["code"] == 0 or not item["text"]:
        raise ValueError(f"step {report['step']}: {call} was not refused")
    if because is not None and because not in item["text"]:
        raise ValueError(
            f"step {report['step']}: {call} was refused, but not for {because}: "
            + item["text"]
        )
    return item["text"]


def boot_identity(text):
    """(slot, marker) the guest witnessed on this boot, if it got that far."""
    slots = set(re.findall(r'"slot": "(root-[ab])"', text))
    if "FLOWERLY_GENERATION_ROOT_VERIFIED" not in text or len(slots) != 1:
        return None, None
    markers = set(MARKER.findall(text))
    if len(markers) > 1:
        raise ValueError("more than one user-data marker in one boot")
    return slots.pop(), (markers.pop() if markers else None)


# ---- host side ---------------------------------------------------------------


def tool_path():
    return os.environ.get("PATH", "") + ":/usr/sbin:/sbin"


def mtools(*argv, data=None):
    program = shutil.which(argv[0], path=tool_path())
    if not program:
        raise ValueError("missing real host tool: " + argv[0])
    done = subprocess.run(
        [program, *argv[1:]], input=data, capture_output=True, check=False
    )
    if done.returncode != 0:
        raise ValueError(f"{argv[0]} failed: {done.stderr.decode()[:200]}")
    return done.stdout


class Esp:
    """The ESP of an offline disk, copied out, edited with mtools, written back."""

    def __init__(self, image):
        self.image = image

    def path(self, name):
        return "::" + name

    def list(self, directory):
        out = mtools("mdir", "-b", "-i", str(self.image), self.path(directory))
        return sorted(
            line.rsplit("/", 1)[-1] for line in out.decode().splitlines() if line
        )

    def read(self, name):
        return mtools("mtype", "-i", str(self.image), self.path(name))

    def write(self, name, data):
        with tempfile.NamedTemporaryFile(dir=self.image.parent) as temp:
            temp.write(data)
            temp.flush()
            mtools("mcopy", "-o", "-i", str(self.image), temp.name, self.path(name))
        if self.read(name) != data:
            raise ValueError(f"ESP {name} read back differently")

    def rename(self, old, new):
        mtools("mren", "-i", str(self.image), self.path(old), self.path(new))

    def delete(self, name):
        mtools("mdel", "-i", str(self.image), self.path(name))


@contextlib.contextmanager
def esp_of(disk, manifest):
    part = next(p for p in manifest["partitions"] if p["name"] == "esp")
    with tempfile.TemporaryDirectory(dir=Path(disk).parent) as work:
        image = Path(work) / "esp.img"
        with Path(disk).open("rb") as source, image.open("wb") as sink:
            source.seek(part["offset_bytes"])
            sink.write(source.read(part["size_bytes"]))
        yield Esp(image)
        data = image.read_bytes()
        if len(data) != part["size_bytes"]:
            raise ValueError("ESP image changed size")
        with Path(disk).open("r+b") as sink:
            sink.seek(part["offset_bytes"])
            sink.write(data)
            sink.flush()
            os.fsync(sink.fileno())


def validate(args, *, actual_execution=False):
    if not COMMIT.fullmatch(args.approved_sha):
        raise ValueError("exact lowercase commit SHA required")
    head = subprocess.run(
        ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
        text=True,
        capture_output=True,
        check=True,
    ).stdout.strip()
    if head != args.approved_sha:
        raise ValueError("approved SHA does not match checked-out source")
    if actual_execution:
        dirty = subprocess.run(
            ["git", "-C", str(ROOT), "status", "--porcelain"],
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        if dirty:
            raise ValueError("dirty git checkout must not execute KVM")
        if os.geteuid() == 0:
            raise ValueError("do not run the KVM acceptance as root")
        if os.environ.get("FLOWERLY_GATE_D_KVM") != TOKEN:
            raise ValueError("explicit Gate D KVM environment approval missing")
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            raise ValueError("usable real /dev/kvm required; no TCG fallback")
        for program in ("qemu-system-x86_64", "mdir", "mtype", "mcopy", "mren", "mdel"):
            if not shutil.which(program, path=tool_path()):
                raise ValueError("missing real host tool: " + program)
    for name in (
        "disk",
        "manifest",
        "kernel",
        "initramfs",
        "candidate_b2",
        "identity_b2",
        "ovmf_code",
        "ovmf_vars",
        "slot_tool",
    ):
        path = getattr(args, name)
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError("missing or unsafe input: " + name)
    if not os.access(args.slot_tool, os.X_OK):
        raise ValueError("--slot-tool must be executable")
    if (
        args.output.exists()
        or args.output.is_symlink()
        or args.output.parent.is_symlink()
    ):
        raise ValueError("refusing to overwrite or follow the evidence directory")
    if not 30 <= args.timeout <= 600 or not 120 <= args.drill_timeout <= 1800:
        raise ValueError("unsafe timeout configuration")
    if not 1 <= args.tries <= 9:
        raise ValueError("unsafe boot-count tries")
    if not UUID.fullmatch(args.root_a) or not UUID.fullmatch(args.root_b):
        raise ValueError("invalid root UUIDs")
    manifest = json.loads(args.manifest.read_text())
    uuids = {p["name"]: p["partuuid"] for p in manifest["partitions"]}
    if (uuids.get("root-a"), uuids.get("root-b")) != (args.root_a, args.root_b):
        raise ValueError("approved root UUIDs do not match the disk")
    # Both slots must already carry what the broker re-verifies.
    registration(manifest, "root-a", "A")
    registration(manifest, "root-b", "B")
    ident = json.loads(args.identity_b2.read_text())
    if ident.get("slot") != "root-b" or ident.get("root_partuuid") != args.root_b:
        raise ValueError("--identity-b2 must be slot B's identity")
    return manifest


class Run:
    def __init__(self, args, manifest):
        self.args = args
        self.manifest = manifest
        self.out = args.output
        self.ab = load("generation_ab_kvm", "scripts/generation-ab-kvm-acceptance.py")
        self.result = {
            "schema": "flowerly-gate-d-kvm/v1",
            "source_sha": args.approved_sha,
            "release_acceptance": False,
            "sequence_verified": False,
            "scenarios": {name: "NOT_RUN" for name in SCENARIOS},
            "boots": [],
            "drill_reports": {},
            "checks": {},
            "slot_tool_sha256": sha_file(args.slot_tool),
            "disk_sha256_initial": sha_file(args.disk),
            "manifest_sha256_initial": sha_file(args.manifest),
            "candidate_b2_sha256": sha_file(args.candidate_b2),
            "ovmf_code_sha256": sha_file(args.ovmf_code),
            "ovmf_vars_template_sha256": sha_file(args.ovmf_vars),
            "root_a_partuuid": args.root_a,
            "root_b_partuuid": args.root_b,
            "boot_tries": args.tries,
            "status": "INCOMPLETE",
        }
        self.disk = self.out / "gate-d-disk.img"
        self.manifest_file = self.out / "gate-d-disk.manifest.json"
        self.vars = self.out / "OVMF_VARS.fd"
        self.marker = None

    # -- disk ------------------------------------------------------------------

    def slot(self, verb, *extra):
        done = subprocess.run(
            [
                str(self.args.slot_tool),
                "slot",
                verb,
                "--disk",
                str(self.disk),
                "--manifest",
                str(self.manifest_file),
                *map(str, extra),
            ],
            capture_output=True,
            text=True,
            env={**os.environ, "PATH": tool_path()},
            timeout=3600,
        )
        if done.returncode != 0:
            raise ValueError(f"slot {verb} failed: {done.stderr.strip()[:240]}")
        return json.loads(done.stdout)

    def publish_manifest(self):
        """The broker's copy of the manifest, on the ESP it reads."""
        self.manifest = json.loads(self.manifest_file.read_text())
        with esp_of(self.disk, self.manifest) as esp:
            esp.write(ESP_MANIFEST, self.manifest_file.read_bytes())

    def instruct(self, doc):
        with esp_of(self.disk, self.manifest) as esp:
            esp.write(INSTRUCTION, json.dumps(doc, sort_keys=True).encode())
        (self.out / f"instruction-step-{doc['step']}.json").write_text(
            json.dumps(doc, indent=2, sort_keys=True) + "\n"
        )

    def entries(self, disk=None):
        with esp_of(disk or self.disk, self.manifest) as esp:
            return esp.list("/loader/entries")

    # -- boots -----------------------------------------------------------------

    def qemu_command(self, disk, vars_file):
        return [
            shutil.which("qemu-system-x86_64"),
            "-machine",
            "q35,accel=kvm",
            "-cpu",
            "host",
            "-smp",
            "2",
            "-m",
            "2048",
            "-nodefaults",
            "-display",
            "none",
            "-serial",
            "stdio",
            "-monitor",
            "none",
            "-net",
            "none",
            "-no-reboot",
            "-drive",
            "if=pflash,format=raw,readonly=on,file=" + str(self.args.ovmf_code),
            "-drive",
            "if=pflash,format=raw,file=" + str(vars_file),
            "-drive",
            "if=virtio,format=raw,cache=none,file=" + str(disk),
        ]

    def check_marker(self, marker, label):
        if marker is None:
            raise ValueError(f"{label}: no user-data marker observed")
        if self.marker is None:
            self.marker = marker
        elif marker != self.marker:
            raise ValueError(f"{label}: user-data marker changed")

    def drill_boot(self, label, step, slot, *, reboot, health="blessed", disk=None):
        """One firmware boot that runs drill ``step`` and reports it.

        health: "blessed" (the health gate passed), "failed" (flowerlyd could
        not start before the drill ran) or None (not checked).
        """
        disk = disk or self.disk
        log = self.out / f"{label}.serial.log"
        command = self.qemu_command(disk, self.vars)
        started = time.monotonic()
        host = None
        with log.open("xb") as output:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                report = None
                while time.monotonic() - started <= self.args.drill_timeout:
                    text = log.read_text(encoding="utf-8", errors="replace")
                    if host is None and "FLOWERLY_GENERATION_ROOT_VERIFIED" in text:
                        host = self.ab.verify_running_qemu(process.pid, command)
                    for word in ("IDLE", "SKIPPED", "REFUSED"):
                        if "FLOWERLY_G5_DRILL_" + word in text:
                            raise ValueError(f"{label}: drill {word.lower()}")
                    report = parse_drill(text, step)
                    if report is not None:
                        break
                    if process.poll() is not None:
                        raise ValueError(
                            f"{label}: QEMU exited {process.returncode} before the drill"
                        )
                    time.sleep(1)
                if report is None:
                    raise ValueError(f"{label}: no drill report in time")
                if host is None:
                    raise ValueError(f"{label}: KVM process never verified")
                if reboot and report["ok"]:
                    try:
                        process.wait(timeout=180)
                    except subprocess.TimeoutExpired:
                        raise ValueError(f"{label}: guest did not reboot") from None
                    if process.returncode != 0:
                        raise ValueError(f"{label}: QEMU exited {process.returncode}")
                else:
                    time.sleep(2)
            finally:
                if process.poll() is None:
                    process.kill()  # deliberate power loss after the report
                process.wait(timeout=15)
        text = log.read_text(encoding="utf-8", errors="replace")
        booted, marker = boot_identity(text)
        if booted != slot:
            raise ValueError(f"{label}: booted {booted}, expected {slot}")
        blessed = "FLOWERLY_GENERATION_BOOT_BLESSED" in text
        if health == "blessed" and (not blessed or supervisor_failed(text)):
            raise ValueError(f"{label}: health gate did not pass")
        if health == "failed" and not supervisor_failed(text):
            raise ValueError(f"{label}: flowerlyd unexpectedly started")
        self.check_marker(marker, label)
        if reboot and report["ok"] and "FLOWERLY_G5_DRILL_REBOOT" not in text:
            raise ValueError(f"{label}: the drill did not ask for its reboot")
        (self.out / f"drill-step-{step}.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n"
        )
        self.result["drill_reports"][str(step)] = sha_file(
            self.out / f"drill-step-{step}.json"
        )
        self.result["boots"].append(
            {
                "label": label,
                "slot": booted,
                "drill_step": step,
                "drill_ok": report["ok"],
                "marker_sha256": marker,
                "health": "BLESSED" if blessed else "NOT_BLESSED",
                "reboot": bool(reboot and report["ok"]),
                "seconds": round(time.monotonic() - started, 3),
                "serial": log.name,
                **host,
            }
        )
        if not report["ok"]:
            raise ValueError(f"{label}: drill step {step} failed; see {log.name}")
        if not history_readable(report):
            raise ValueError(f"{label}: history unreadable after step {step}")
        self.ab.verify_immutable_roots(disk, self.manifest)
        return report

    def unhealthy_boot(self, label, slot):
        record = self.ab.qemu_boot(
            self.args, self.disk, self.vars, self.out, slot, bless=False, label=label
        )
        self.check_marker(record["marker_sha256"], label)
        self.result["boots"].append(record)

    def observe(self, label, disk, vars_file):
        """Boot ``disk`` and record what the loader chose; expect nothing."""
        log = self.out / f"{label}.serial.log"
        command = self.qemu_command(disk, vars_file)
        started = time.monotonic()
        host = None
        with log.open("xb") as output:
            process = subprocess.Popen(
                command,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                while time.monotonic() - started <= OBSERVE_SECONDS:
                    if process.poll() is not None:
                        break
                    # Right after Popen the exec may not have completed and
                    # /proc/<pid>/cmdline reads empty (run 4); once the
                    # firmware writes, the process is QEMU.
                    if host is None and log.stat().st_size > 0:
                        host = self.ab.verify_running_qemu(process.pid, command)
                    time.sleep(1)
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=15)
        if host is None:
            raise ValueError(f"{label}: the running QEMU was not verified")
        text = log.read_text(encoding="utf-8", errors="replace")
        booted, _ = boot_identity(text)
        initrd = sorted(set(re.findall(r"initrd=\\flowerly\\([ab])\\initramfs", text)))
        record = {
            "label": label,
            "booted_slot": booted,
            "loader_started_slots": initrd,
            "qemu_exit": process.returncode,
            "seconds": round(time.monotonic() - started, 3),
            "serial": log.name,
            **(host or {}),
        }
        self.result["boots"].append(record)
        return record

    # -- the sequence ----------------------------------------------------------

    def main_chain(self):
        a, b = "root-a", "root-b"
        scen = self.result["scenarios"]
        checks = self.result["checks"]
        tries = self.args.tries
        self.instruct(
            instruction(
                1,
                a,
                [
                    registration(self.manifest, a, "A"),
                    registration(self.manifest, b, "B"),
                ],
                False,
            )
        )
        r1 = self.drill_boot("step-1-a-register", 1, a, reboot=False)
        ga, gb = registered(r1, "A"), registered(r1, "B")
        scen["register-tested"] = "PASS"
        self.result["generations"] = {"A": ga, "B": gb}

        # The forced-unhealthy activation comes first, while B is still
        # TESTED: the acknowledgement rule (ADR-0023 Q4) guards activations.
        self.instruct(
            instruction(
                2, a, [{"op": "activate", "generation": gb, "expect": "armed"}], True
            )
        )
        self.drill_boot("step-2-a-activate-b", 2, a, reboot=True)
        self.make_unhealthy("b")
        self.instruct(
            instruction(
                3,
                a,
                [{"op": "activate", "generation": gb, "expect": "refused"}],
                False,
            )
        )
        for index in range(tries):
            self.unhealthy_boot(f"step-2-b-unhealthy-{index + 1}", b)
        r3 = self.drill_boot("step-3-a-fell-back", 3, a, reboot=False)
        fell = last_activation(r3, gb)
        if fell["state"] != "FELL_BACK":
            raise ValueError(f"B's activation is {fell['state']}, not FELL_BACK")
        scen["unhealthy-activation-fell-back"] = "PASS"
        checks["fell_back_refusal"] = refused(r3, "activate", because="FELL_BACK")
        scen["fell-back-needs-acknowledgement"] = "PASS"

        self.instruct(
            instruction(
                4,
                a,
                [
                    {
                        "op": "activate",
                        "generation": gb,
                        "acknowledges": fell["id"],
                        "expect": "armed",
                    }
                ],
                True,
            )
        )
        self.drill_boot("step-4-a-acknowledged", 4, a, reboot=True)
        self.instruct(
            instruction(
                5, b, [{"op": "activate", "generation": ga, "expect": "armed"}], True
            )
        )
        r5 = self.drill_boot("step-5-b-confirmed", 5, b, reboot=True)
        again = last_activation(r5, gb)
        if not is_confirmed(r5, gb, "ACTIVATE") or again["acknowledges"] != fell["id"]:
            raise ValueError("the acknowledged activation of B was not CONFIRMED")
        scen["activate-b-confirmed"] = "PASS"
        scen["acknowledged-reactivation-confirmed"] = "PASS"

        self.instruct(
            instruction(
                6, a, [{"op": "rollback", "generation": gb, "expect": "armed"}], True
            )
        )
        r6 = self.drill_boot("step-6-a-confirmed", 6, a, reboot=True)
        if not is_confirmed(r6, ga, "ACTIVATE"):
            raise ValueError("A's activation was not CONFIRMED")
        scen["activate-a-confirmed"] = "PASS"

        self.instruct(
            instruction(
                7, b, [{"op": "rollback", "generation": ga, "expect": "armed"}], True
            )
        )
        r7 = self.drill_boot("step-7-b-rollback-confirmed", 7, b, reboot=True)
        if not is_confirmed(r7, gb, "ROLLBACK"):
            raise ValueError("the rollback to B was not CONFIRMED")
        scen["rollback-b-confirmed"] = "PASS"

        # The rollback to A is armed; make its boot fail the health gate. A
        # rollback that fell back needs no acknowledgement to be retried.
        self.make_unhealthy("a")
        self.instruct(
            instruction(
                8, b, [{"op": "rollback", "generation": ga, "expect": "armed"}], True
            )
        )
        for index in range(tries):
            self.unhealthy_boot(f"step-7-a-unhealthy-{index + 1}", a)
        r8 = self.drill_boot("step-8-b-fell-back", 8, b, reboot=True)
        rows = [row for row in activations(r8) if row["generation"] == ga]
        rollbacks = [row for row in rows if row["kind"] == "ROLLBACK"]
        if len(rollbacks) < 2 or rollbacks[-2]["state"] != "FELL_BACK":
            raise ValueError("A's rollback did not fall back before the retry")
        scen["unhealthy-rollback-fell-back"] = "PASS"
        checks["rollback_retry_without_acknowledgement"] = rollbacks[-1]["id"]

        self.instruct(instruction(9, a, [{"op": "inspect", "generation": ga}], False))
        r9 = self.drill_boot("step-9-a-confirmed", 9, a, reboot=False)
        if not is_confirmed(r9, ga, "ROLLBACK"):
            raise ValueError("the retried rollback to A was not CONFIRMED")
        return ga

    def make_unhealthy(self, letter):
        """Make the broker-armed entry of slot ``letter`` fail the health gate."""
        armed = f"flowerly-{letter}+{self.args.tries}.conf"
        with esp_of(self.disk, self.manifest) as esp:
            if armed not in esp.list("/loader/entries"):
                raise ValueError(f"the broker did not arm {letter.upper()}")
            path = "/loader/entries/" + armed
            esp.write(path, with_option(esp.read(path).decode(), FAIL_HEALTH).encode())

    def schema_chain(self, ga):
        a, b = "root-a", "root-b"
        scen = self.result["scenarios"]
        checks = self.result["checks"]
        # B's superseded entry goes before B2 is staged in its slot.
        self.slot("rollback", "--active-partuuid", self.args.root_a, "--to", a)
        self.slot(
            "stage",
            "--active-partuuid",
            self.args.root_a,
            "--slot",
            b,
            "--candidate",
            self.args.candidate_b2,
            "--sha256",
            self.result["candidate_b2_sha256"],
            "--kernel",
            self.args.kernel,
            "--initramfs",
            self.args.initramfs,
            "--identity",
            self.args.identity_b2,
        )
        self.publish_manifest()
        self.instruct(
            instruction(
                10,
                a,
                [registration(self.manifest, b, "B2")],
                False,
            )
        )
        r10 = self.drill_boot("step-10-a-register-b2", 10, a, reboot=False)
        gb2 = registered(r10, "B2")
        self.result["generations"]["B2"] = gb2
        self.instruct(
            instruction(
                11,
                a,
                [
                    {"op": "activate", "generation": gb2, "expect": "refused"},
                    {
                        "op": "activate",
                        "generation": gb2,
                        "acknowledge_irreversible": True,
                        "expect": "armed",
                    },
                ],
                True,
            )
        )
        r11 = self.drill_boot("step-11-a-irreversible", 11, a, reboot=True)
        checks["irreversible_refusal"] = refused(
            r11, "activate", 0, because="irreversible"
        )
        armed = last_activation(r11, gb2)
        if not armed["irreversible"] or not armed["backup_sha256"]:
            raise ValueError("the irreversible activation recorded no backup")
        scen["irreversible-needs-acknowledgement"] = "PASS"
        if "manual-flowerly-a.conf" not in self.entries():
            raise ValueError("A's entry was not demoted to manual-only")

        self.instruct(
            instruction(
                12,
                b,
                [{"op": "rollback", "generation": ga, "expect": "refused"}],
                False,
            )
        )
        r12 = self.drill_boot("step-12-b2-confirmed", 12, b, reboot=False)
        if not is_confirmed(r12, gb2, "ACTIVATE"):
            raise ValueError("B2's irreversible activation was not CONFIRMED")
        scen["irreversible-activation-confirmed"] = "PASS"
        checks["downgrade_refusal"] = refused(
            r12, "rollback", because="cannot read the database"
        )
        if "cannot read" not in checks["downgrade_refusal"]:
            raise ValueError("the rollback was refused for another reason")
        scen["downgrade-refused-across-schema"] = "PASS"

        # What systemd-boot does when the only default match is exhausted.
        copy = self.out / "exhausted-target.img"
        shutil.copyfile(self.disk, copy)
        with esp_of(copy, self.manifest) as esp:
            names = esp.list("/loader/entries")
            if "flowerly-b.conf" not in names:
                raise ValueError(f"unexpected entries before the observation: {names}")
            esp.rename(
                "/loader/entries/flowerly-b.conf",
                f"/loader/entries/flowerly-b+0-{self.args.tries}.conf",
            )
            esp.delete(INSTRUCTION)
            names = esp.list("/loader/entries")
        vars_copy = self.out / "OVMF_VARS-exhausted.fd"
        shutil.copyfile(self.vars, vars_copy)
        seen = self.observe("exhausted-target", copy, vars_copy)
        seen["entries"] = names
        checks["exhausted_target"] = seen
        scen["exhausted-target-observed"] = "PASS"
        copy.unlink()

        # The operator picks the demoted A in the boot menu (loader.conf here)
        # and restores the backup flowerlyd took before the irreversible arm.
        with esp_of(self.disk, self.manifest) as esp:
            loader = esp.read("/loader/loader.conf").decode()
            esp.write(
                "/loader/loader.conf",
                with_default(loader, "manual-flowerly-a.conf").encode(),
            )
        self.instruct(
            instruction(
                13,
                a,
                [
                    {"op": "restore", "backup_sha256": armed["backup_sha256"]},
                    {"op": "inspect", "generation": ga},
                ],
                False,
            )
        )
        r13 = self.drill_boot("step-13-a-restore", 13, a, reboot=False, health="failed")
        scen["older-root-cannot-read-the-database"] = "PASS"
        rows = activations(r13)
        if any(row["generation"] == gb2 for row in rows):
            raise ValueError("the restored database still holds B2's activation")
        if last_activation(r13, ga)["state"] != "CONFIRMED":
            raise ValueError("the restored history lost A's activation")
        scen["restore-drill"] = "PASS"

    def run(self):
        self.out.mkdir(mode=0o700)
        shutil.copyfile(self.args.disk, self.disk)
        shutil.copyfile(self.args.manifest, self.manifest_file)
        shutil.copyfile(self.args.ovmf_vars, self.vars)
        file = self.out / "acceptance.json"
        try:
            self.result["qemu_version"] = subprocess.run(
                [shutil.which("qemu-system-x86_64"), "--version"],
                capture_output=True,
                text=True,
                check=True,
            ).stdout.splitlines()[0][:160]
            self.publish_manifest()
            ga = self.main_chain()
            self.schema_chain(ga)
            self.result["scenarios"]["user-data-intact"] = "PASS"
            self.result["scenarios"]["history-readable-every-step"] = "PASS"
            self.result["user_marker_sha256"] = self.marker
            self.result["sequence_verified"] = all(
                v == "PASS" for v in self.result["scenarios"].values()
            )
            self.result["status"] = (
                "GATE_D_SEQUENCE_OBSERVED"
                if self.result["sequence_verified"]
                else "INCOMPLETE"
            )
        except (
            ValueError,
            KeyError,
            OSError,
            subprocess.CalledProcessError,
            subprocess.TimeoutExpired,
        ) as error:
            self.result["status"] = "FAILED_OR_INCONCLUSIVE"
            self.result["error"] = str(error)[:400]
            raise
        finally:
            if self.disk.is_file():
                self.result["final_entries"] = self.entries()
                self.result["disk_sha256_final"] = sha_file(self.disk)
            file.write_text(json.dumps(self.result, indent=2, sort_keys=True) + "\n")
            hashes = {
                p.name: sha_file(p) for p in sorted(self.out.glob("*.serial.log"))
            }
            (self.out / "serial-sha256.json").write_text(
                json.dumps(hashes, indent=2, sort_keys=True) + "\n"
            )
        return self.result


def parser():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--execute", action="store_true")
    p.add_argument("--approved-sha", required=True)
    for name in (
        "disk",
        "manifest",
        "kernel",
        "initramfs",
        "candidate-b2",
        "identity-b2",
        "ovmf-code",
        "ovmf-vars",
        "slot-tool",
        "output",
    ):
        p.add_argument(
            "--" + name, dest=name.replace("-", "_"), type=Path, required=True
        )
    p.add_argument("--root-a", required=True)
    p.add_argument("--root-b", required=True)
    p.add_argument("--tries", type=int, default=3)
    p.add_argument("--timeout", type=int, default=180)
    p.add_argument("--drill-timeout", type=int, default=900)
    return p


def main():
    args = parser().parse_args()
    try:
        if args.execute:
            manifest = validate(args, actual_execution=True)
            print(
                "Starting explicitly authorized real Gate D KVM acceptance", flush=True
            )
            result = Run(args, manifest).run()
            print(
                json.dumps(
                    {
                        "status": result["status"],
                        "sequence_verified": result["sequence_verified"],
                        "release_acceptance": False,
                    },
                    sort_keys=True,
                )
            )
        else:
            validate(args)
            print("VALIDATED ONLY; Gate D KVM NOT_RUN.")
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"Gate D KVM refusal/failure: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

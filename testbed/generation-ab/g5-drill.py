#!/usr/bin/env python3
"""Gate D (ADR-0023 G5) Admin drill, A/B test image only.

The KVM host cannot type into the guest, so it writes one instruction per
boot to the ESP (offline, with mtools) and reads the outcome back from the
serial console. This service runs the instruction's Admin steps through the
real agenticctl -> flowerlyd -> broker path as an Admin account, never as root
and never by writing the database itself. It holds no state: Generation ids
come back in the report and the host names them in the next instruction.

The report is a base64 JSON document split into console lines:

    FLOWERLY_G5_DRILL step=N part=i/n <base64>
    FLOWERLY_G5_DRILL_DONE step=N parts=n sha256=<hex of the JSON> ok=true|false

A failed expectation stops the remaining actions and the requested reboot.
"""

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

INSTRUCTION = Path("/efi/flowerly/g5-drill.json")
RUNNING_IDENTITY = Path("/usr/lib/flowerly/generation-root.json")
SCHEMA = "flowerly-g5-drill/v1"
ADMIN = "flowerly-drill"
AGENTICCTL = ["/usr/bin/agenticctl", "--runtime-dir", "/run/flowerly", "generation"]
MAX_INSTRUCTION = 64 * 1024
CHUNK = 480
SLOTS = ("root-a", "root-b")
HEX64 = re.compile(r"[0-9a-f]{64}")
HEX40 = re.compile(r"[0-9a-f]{40}")
GENERATION = re.compile(r"[A-Za-z0-9_-]{1,128}")
BUILD_ID = re.compile(r"[A-Za-z0-9_.-]{1,64}")
IDENTITY_KEYS = {
    "source_commit",
    "rootfs_sha256",
    "kernel_sha256",
    "initramfs_sha256",
    "input_manifest_sha256",
    "build_id",
}
ACTION_KEYS = {
    "register": {"op", "label", "identity", "evidence_sha256"},
    "activate": {
        "op",
        "generation",
        "acknowledges",
        "acknowledge_irreversible",
        "expect",
    },
    "rollback": {"op", "generation", "acknowledge_irreversible", "expect"},
    "inspect": {"op", "generation"},
}
EXPECT = ("armed", "refused")


def _keys(obj, allowed, required, what):
    if not isinstance(obj, dict):
        raise ValueError(what + " is not an object")
    if set(obj) - allowed:
        raise ValueError(
            what + " has unknown keys: " + ",".join(sorted(set(obj) - allowed))
        )
    if required - set(obj):
        raise ValueError(
            what + " misses keys: " + ",".join(sorted(required - set(obj)))
        )


def _generation(value, what):
    if not isinstance(value, str) or not GENERATION.fullmatch(value):
        raise ValueError(what + " needs a Generation id")


def validate_action(action, index):
    what = "action %d" % index
    if not isinstance(action, dict) or action.get("op") not in ACTION_KEYS:
        raise ValueError(what + " has no known op")
    op = action["op"]
    required = {"op"} | {
        "register": {"label", "identity", "evidence_sha256"},
        "activate": {"generation", "expect"},
        "rollback": {"generation", "expect"},
        "inspect": {"generation"},
    }[op]
    _keys(action, ACTION_KEYS[op], required, what)
    if op == "register":
        if not isinstance(action["label"], str) or not BUILD_ID.fullmatch(
            action["label"]
        ):
            raise ValueError(what + " needs a plain label")
        identity = action["identity"]
        _keys(identity, IDENTITY_KEYS, IDENTITY_KEYS, what + " identity")
        if not isinstance(identity["source_commit"], str) or not HEX40.fullmatch(
            identity["source_commit"]
        ):
            raise ValueError(what + " identity needs a full source commit")
        for key in (
            "rootfs_sha256",
            "kernel_sha256",
            "initramfs_sha256",
            "input_manifest_sha256",
        ):
            if not isinstance(identity[key], str) or not HEX64.fullmatch(identity[key]):
                raise ValueError(what + " identity needs a SHA-256 " + key)
        if not isinstance(identity["build_id"], str) or not BUILD_ID.fullmatch(
            identity["build_id"]
        ):
            raise ValueError(what + " identity needs a build id")
        if not isinstance(action["evidence_sha256"], str) or not HEX64.fullmatch(
            action["evidence_sha256"]
        ):
            raise ValueError(what + " needs an evidence SHA-256")
        return
    _generation(action["generation"], what)
    if op == "inspect":
        return
    if action["expect"] not in EXPECT:
        raise ValueError(what + " expects armed or refused")
    if not isinstance(action.get("acknowledge_irreversible", False), bool):
        raise ValueError(what + " acknowledge_irreversible is not a boolean")
    ack = action.get("acknowledges")
    if ack is not None and (type(ack) is not int or ack < 1):
        raise ValueError(what + " acknowledges is not an activation id")


def load_instruction(raw):
    """Parse and validate the host's instruction; refuse anything unexpected."""
    try:
        doc = json.loads(raw)
    except ValueError as error:
        raise ValueError("instruction is not JSON") from error
    _keys(
        doc,
        {"schema", "step", "slot", "actions", "reboot"},
        {"schema", "step", "slot", "actions", "reboot"},
        "instruction",
    )
    if doc["schema"] != SCHEMA:
        raise ValueError("instruction schema is not " + SCHEMA)
    if type(doc["step"]) is not int or not 1 <= doc["step"] <= 999:
        raise ValueError("instruction step is not 1..999")
    if doc["slot"] not in SLOTS:
        raise ValueError("instruction slot is not root-a or root-b")
    if not isinstance(doc["reboot"], bool):
        raise ValueError("instruction reboot is not a boolean")
    if not isinstance(doc["actions"], list) or len(doc["actions"]) > 16:
        raise ValueError("instruction actions is not a list of at most 16")
    for index, action in enumerate(doc["actions"]):
        validate_action(action, index)
    return doc


def _reply(result):
    """(code, parsed stdout or None, text) of one agenticctl run."""
    code, out, err = result
    try:
        value = json.loads(out) if out.strip() else None
    except ValueError:
        value = None
    return code, value, (err or out).strip()[-2000:]


def _record(entries, name, argv, result):
    code, value, text = _reply(result)
    entries.append(
        {"call": name, "argv": argv, "code": code, "reply": value, "text": text}
    )
    return code, value


def run_actions(doc, ctl):
    """Run the actions with ``ctl(argv) -> (code, stdout, stderr)``.

    Returns (ok, entries). The first unmet expectation stops the rest.
    """
    entries = []
    for action in doc["actions"]:
        op = action["op"]
        if op == "register":
            ident = action["identity"]
            label = action["label"]
            argv = ["create", "--source-commit", ident["source_commit"]]
            code, created = _record(entries, "create", argv, ctl(argv))
            if code != 0 or not isinstance(created, dict) or "id" not in created:
                return False, entries
            gid = created["id"]
            steps = [
                ("promote", ["promote", gid, "--reason", "G5 drill: share " + label]),
                (
                    "identity",
                    ["identity", gid]
                    + [
                        part
                        for key in (
                            "source_commit",
                            "rootfs_sha256",
                            "kernel_sha256",
                            "initramfs_sha256",
                            "input_manifest_sha256",
                            "build_id",
                        )
                        for part in ("--" + key.replace("_", "-"), ident[key])
                    ]
                    + ["--reason", "G5 drill: identity of " + label],
                ),
                (
                    "evidence",
                    [
                        "evidence",
                        gid,
                        "--outcome",
                        "passed",
                        "--evidence-sha256",
                        action["evidence_sha256"],
                        "--reason",
                        "G5 drill: acceptance of " + label,
                    ],
                ),
            ]
            for name, argv in steps:
                code, value = _record(entries, name, argv, ctl(argv))
                if code != 0:
                    return False, entries
            experiment = value.get("experiment") if isinstance(value, dict) else None
            if not isinstance(experiment, str) or not experiment:
                return False, entries
            argv = [
                "test",
                gid,
                "--experiment",
                experiment,
                "--reason",
                "G5 drill: TESTED " + label,
            ]
            code, tested = _record(entries, "test", argv, ctl(argv))
            if (
                code != 0
                or not isinstance(tested, dict)
                or tested.get("status") != "TESTED"
            ):
                return False, entries
            entries.append({"call": "registered", "label": label, "generation": gid})
        elif op == "inspect":
            argv = ["inspect", action["generation"]]
            code, _ = _record(entries, "inspect", argv, ctl(argv))
            if code != 0:
                return False, entries
        else:
            argv = [
                op,
                action["generation"],
                "--reason",
                "G5 drill step %d" % doc["step"],
            ]
            if action.get("acknowledges") is not None:
                argv += ["--acknowledges", str(action["acknowledges"])]
            if action.get("acknowledge_irreversible"):
                argv.append("--acknowledge-irreversible")
            code, value = _record(entries, op, argv, ctl(argv))
            armed = code == 0 and isinstance(value, dict) and "activation" in value
            if (action["expect"] == "armed") != armed or (
                action["expect"] == "refused" and code == 0
            ):
                return False, entries
    return True, entries


def report_lines(step, report):
    """Console lines carrying ``report``; the DONE line names its digest."""
    body = json.dumps(report, sort_keys=True, separators=(",", ":")).encode()
    encoded = base64.b64encode(body).decode()
    chunks = [encoded[i : i + CHUNK] for i in range(0, len(encoded), CHUNK)] or [""]
    lines = [
        "FLOWERLY_G5_DRILL step=%d part=%d/%d %s" % (step, i + 1, len(chunks), chunk)
        for i, chunk in enumerate(chunks)
    ]
    lines.append(
        "FLOWERLY_G5_DRILL_DONE step=%d parts=%d sha256=%s ok=%s"
        % (
            step,
            len(chunks),
            hashlib.sha256(body).hexdigest(),
            str(report["ok"]).lower(),
        )
    )
    return lines


def agenticctl(argv):
    result = subprocess.run(
        ["runuser", "-u", ADMIN, "--"] + AGENTICCTL + argv,
        capture_output=True,
        text=True,
        timeout=420,
        check=False,
    )
    return result.returncode, result.stdout, result.stderr


def drill(instruction, running_identity, ctl, emit):
    """One boot. Returns True when the host asked for a reboot and it is due."""
    if not instruction.exists():
        emit("FLOWERLY_G5_DRILL_IDLE")
        return False
    meta = os.lstat(instruction)
    if (
        not os.path.isfile(instruction)
        or os.path.islink(instruction)
        or meta.st_size > MAX_INSTRUCTION
    ):
        emit("FLOWERLY_G5_DRILL_REFUSED instruction is not a small regular file")
        return False
    try:
        doc = load_instruction(instruction.read_text())
    except ValueError as error:
        emit("FLOWERLY_G5_DRILL_REFUSED " + str(error))
        return False
    slot = json.loads(running_identity.read_text()).get("slot")
    if slot != doc["slot"]:
        emit(
            "FLOWERLY_G5_DRILL_SKIPPED step=%d running=%s wanted=%s"
            % (doc["step"], slot, doc["slot"])
        )
        return False
    ok, entries = run_actions(doc, ctl)
    # History must stay readable after every step, also after a failure.
    for name, argv in (("list", ["list"]), ("history", ["history"])):
        _record(entries, name, argv, ctl(argv))
    report = {
        "schema": SCHEMA + "-report",
        "step": doc["step"],
        "slot": slot,
        "ok": ok,
        "entries": entries,
    }
    for line in report_lines(doc["step"], report):
        emit(line)
    return ok and doc["reboot"]


def main():
    def emit(line):
        print(line, flush=True)

    if drill(INSTRUCTION, RUNNING_IDENTITY, agenticctl, emit):
        emit("FLOWERLY_G5_DRILL_REBOOT")
        subprocess.run(["systemctl", "reboot"], check=False)
    return 0


if __name__ == "__main__":
    sys.exit(main())

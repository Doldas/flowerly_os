#!/usr/bin/env python3
"""Fail-closed consistency audit of Generation A/B KVM serial evidence.

This checks file integrity and self-consistency, not cryptographic
authenticity of QEMU execution or the full eight-scenario S7 release gate.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys

SHA = re.compile(r"[0-9a-f]{64}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")
SCENARIOS = {
    "stage-complete",
    "stage-interrupted",
    "arm-before-rename",
    "arm-after-rename",
    "boot-before-bless",
    "unhealthy-b-fallback",
    "data-survives",
    "tampered-b-refused",
}
MEASURED = {"boot-before-bless", "unhealthy-b-fallback", "data-survives"}


def sha_file(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        while chunk := source.read(1048576):
            digest.update(chunk)
    return digest.hexdigest()


def verify_log(path, slot, partuuid, should_bless):
    target = Path(path)
    if (
        target.is_symlink()
        or not target.is_file()
        or target.stat().st_size > 16 * 1048576
    ):
        raise ValueError("missing or unsafe boot serial evidence")
    text = target.read_text(encoding="utf-8", errors="replace")
    if (
        "Kernel panic" in text
        or "Failed to start Flowerly OS Unified Supervisor" in text
    ):
        raise ValueError("guest error in serial log")
    if text.count("FLOWERLY_GENERATION_ROOT_VERIFIED") != 1:
        raise ValueError("no unique root witness")
    identities = []
    for line in text.splitlines():
        if '"boot_id"' not in line or '"root_partuuid"' not in line:
            continue
        start, stop = line.find("{"), line.rfind("}")
        if start == -1 or stop <= start:
            raise ValueError("invalid guest identity line")
        try:
            identity = json.loads(line[start : stop + 1])
        except (ValueError, TypeError) as error:
            raise ValueError("malformed guest identity") from error
        identities.append(identity)
    if len(identities) != 1:
        raise ValueError("repeated or missing guest identity")
    identity = identities[0]
    if set(identity) != {"boot_id", "root_partuuid", "slot"} or (
        identity["root_partuuid"] != partuuid or identity["slot"] != slot
    ):
        raise ValueError("guest root/EFI evidence mismatch")
    boot_id = identity["boot_id"]
    if not isinstance(boot_id, str) or not UUID.fullmatch(boot_id):
        raise ValueError("malformed kernel boot ID")
    markers = re.findall(r"FLOWERLY_GENERATION_USER_MARKER_SHA256=([0-9a-f]{64})", text)
    if len(markers) != 1:
        raise ValueError("missing or duplicate user-data marker")
    blessed = text.count("FLOWERLY_GENERATION_BOOT_BLESSED")
    ping = text.count("FLOWERLY_GENERATION_IPC_VERIFIED")
    failure = text.count("injected unhealthy Generation boot")
    expected = (1, 1, 0) if should_bless else (0, 0, 1)
    if (blessed, ping, failure) != expected:
        raise ValueError("health/boot blessing contradicts expected slot")
    return {"boot_id": boot_id, "marker": markers[0], "sha256": sha_file(target)}


def audit(report, directory):
    if (
        not isinstance(report, dict)
        or report.get("schema") != "flowerly-generation-kvm-sequence/v1"
    ):
        raise ValueError("unknown KVM evidence schema")
    if (
        report.get("release_acceptance") is not False
        or report.get("sequence_verified") is not True
    ):
        raise ValueError("not an observed nonrelease sequence")
    if report.get("status") != "SEQUENCE_OBSERVED_S7_INCOMPLETE":
        raise ValueError("unexpected KVM verdict")
    if not isinstance(report.get("source_sha"), str) or not COMMIT.fullmatch(
        report["source_sha"]
    ):
        raise ValueError("source SHA not bound")
    for key in (
        "disk_sha256_initial",
        "ovmf_code_sha256",
        "ovmf_vars_template_sha256",
        "manifest_sha256",
    ):
        value = report.get(key)
        if not isinstance(value, str) or not SHA.fullmatch(value):
            raise ValueError("missing artifact provenance: " + key)
    a, b = report.get("root_a_partuuid"), report.get("root_b_partuuid")
    if (
        not isinstance(a, str)
        or not isinstance(b, str)
        or (not UUID.fullmatch(a) or not UUID.fullmatch(b) or a == b)
    ):
        raise ValueError("ambiguous root UUIDs")
    scenarios = report.get("scenarios")
    if not isinstance(scenarios, dict) or set(scenarios) != SCENARIOS:
        raise ValueError("incomplete scenario catalog")
    if any(
        v not in ("PASS", "NOT_RUN", "FAIL", "INCONCLUSIVE") for v in scenarios.values()
    ):
        raise ValueError("invalid scenario verdict")
    if any(scenarios[name] != "PASS" for name in MEASURED) or (
        any(scenarios[name] == "PASS" for name in SCENARIOS - MEASURED)
    ):
        raise ValueError("unobserved scenario claimed PASS")
    tries = report.get("boot_tries")
    boots = report.get("boots")
    if (
        type(tries) is not int
        or not 1 <= tries <= 9
        or (not isinstance(boots, list) or len(boots) != tries + 2)
    ):
        raise ValueError("unverified boot attempt count")
    folder = Path(directory)
    if folder.is_symlink() or not folder.is_dir():
        raise ValueError("unsafe evidence directory")
    seen_ids, names, markers, digests = set(), set(), set(), {}
    for index, boot in enumerate(boots):
        if not isinstance(boot, dict):
            raise ValueError("malformed per-boot report")
        is_b = 0 < index < len(boots) - 1
        slot = "root-b" if is_b else "root-a"
        name = (
            "boot-a.serial.log"
            if index == 0
            else "auto-fallback-a.serial.log"
            if index == len(boots) - 1
            else f"unhealthy-b-{index}.serial.log"
        )
        if boot.get("serial") != name or name in names:
            raise ValueError("boot report/serial name mismatch")
        names.add(name)
        observed = verify_log(folder / name, slot, b if is_b else a, not is_b)
        if observed["boot_id"] in seen_ids:
            raise ValueError("duplicate kernel boot ID")
        seen_ids.add(observed["boot_id"])
        if (
            boot.get("slot") != slot
            or boot.get("marker_sha256") != observed["marker"]
            or (boot.get("observed_health") != ("REJECTED" if is_b else "BLESSED"))
            or boot.get("power_loss") != "QEMU_SIGKILL"
        ):
            raise ValueError("boot report and serial evidence disagree")
        if boot.get("kvm_fd_verified") is not True or (
            not isinstance(boot.get("qemu_cmdline_sha256"), str)
            or not SHA.fullmatch(boot["qemu_cmdline_sha256"])
        ):
            raise ValueError("no actual KVM process evidence for observed boot")
        markers.add(observed["marker"])
        digests[name] = observed["sha256"]
    if len(markers) != 1:
        raise ValueError("persistent marker changed during A/B/A")
    digest_file = folder / "serial-sha256.json"
    if (
        digest_file.is_symlink()
        or not digest_file.is_file()
        or digest_file.stat().st_size > 16384
    ):
        raise ValueError("missing serial checksum manifest")
    if json.loads(digest_file.read_text()) != digests:
        raise ValueError("serial digest manifest does not match")
    if set(p.name for p in folder.glob("*.serial.log")) != names:
        raise ValueError("unaccounted serial artifact")
    return {
        "status": "INTERNALLY_CONSISTENT_NOT_RELEASE_ACCEPTED",
        "boots": len(boots),
        "source_sha": report["source_sha"],
        "marker_sha256": next(iter(markers)),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        folder = args.evidence_dir
        if folder.is_symlink():
            raise ValueError("symlink evidence root refused")
        report_file = folder / "acceptance.json"
        if (
            report_file.is_symlink()
            or not report_file.is_file()
            or report_file.stat().st_size > 1048576
        ):
            raise ValueError("missing, oversized or symlinked acceptance report")
        result = audit(json.loads(report_file.read_text()), folder)
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f"KVM evidence refused: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    sys.exit(main())

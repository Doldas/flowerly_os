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
    "healthy-b-activation",
    "rollback-to-a",
    "damaged-entry-fallback",
    "measure-stage-and-boot",
}
EXPECTATIONS = {"BLESSED", "REJECTED", "NO_WITNESS"}
HOST_CHECKS = {"stage-complete", "stage-interrupted", "tampered-b-refused"}
ENTRY = re.compile(r"Kernel command line: initrd=\\flowerly\\([ab])\\initramfs\.img")


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
    entries = ENTRY.findall(text)
    if entries != [slot[-1]]:
        raise ValueError("loader started a different slot than the witnessed root")
    return {"boot_id": boot_id, "marker": markers[0], "sha256": sha_file(target)}


def verify_quiet_log(path, slot):
    """A boot that must NOT reach the Flowerly root witness (damaged entry)."""
    target = Path(path)
    if (
        target.is_symlink()
        or not target.is_file()
        or target.stat().st_size > 16 * 1048576
    ):
        raise ValueError("missing or unsafe boot serial evidence")
    text = target.read_text(encoding="utf-8", errors="replace")
    if any(
        token in text
        for token in (
            "FLOWERLY_GENERATION_ROOT_VERIFIED",
            "FLOWERLY_GENERATION_BOOT_BLESSED",
            "FLOWERLY_GENERATION_IPC_VERIFIED",
        )
    ):
        raise ValueError("damaged-entry boot unexpectedly reached the Flowerly guest")
    if ENTRY.findall(text) != [slot[-1]]:
        raise ValueError("damaged-entry boot did not attempt the damaged slot")
    return {"boot_id": None, "marker": None, "sha256": sha_file(target)}


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
    tries = report.get("boot_tries")
    boots = report.get("boots")
    if (
        type(tries) is not int
        or not 1 <= tries <= 9
        or not isinstance(boots, list)
        or not boots
    ):
        raise ValueError("unverified boot attempt count")
    folder = Path(directory)
    if folder.is_symlink() or not folder.is_dir():
        raise ValueError("unsafe evidence directory")
    seen_ids, names, digests, by_label, lineage_markers = set(), set(), {}, {}, {}
    for boot in boots:
        if not isinstance(boot, dict):
            raise ValueError("malformed per-boot report")
        label, slot, expect = boot.get("label"), boot.get("slot"), boot.get("expect")
        if (
            not isinstance(label, str)
            or not re.fullmatch(r"[a-z0-9-]{1,48}", label)
            or label in by_label
            or slot not in ("root-a", "root-b")
            or expect not in EXPECTATIONS
        ):
            raise ValueError("malformed or duplicate boot label")
        name = label + ".serial.log"
        if boot.get("serial") != name or name in names:
            raise ValueError("boot report/serial name mismatch")
        names.add(name)
        partuuid = b if slot == "root-b" else a
        if expect == "NO_WITNESS":
            observed = verify_quiet_log(folder / name, slot)
            if boot.get("marker_sha256") is not None:
                raise ValueError("boot report and serial evidence disagree")
        else:
            observed = verify_log(folder / name, slot, partuuid, expect == "BLESSED")
            if observed["boot_id"] in seen_ids:
                raise ValueError("duplicate kernel boot ID")
            seen_ids.add(observed["boot_id"])
            if boot.get("marker_sha256") != observed["marker"]:
                raise ValueError("boot report and serial evidence disagree")
            lineage = boot.get("lineage")
            if not isinstance(lineage, str) or not re.fullmatch(
                r"[a-z0-9-]{1,32}", lineage
            ):
                raise ValueError("missing data lineage")
            lineage_markers.setdefault(lineage, set()).add(observed["marker"])
        if boot.get("power_loss") != "QEMU_SIGKILL":
            raise ValueError("boot report and serial evidence disagree")
        if boot.get("kvm_fd_verified") is not True or (
            not isinstance(boot.get("qemu_cmdline_sha256"), str)
            or not SHA.fullmatch(boot["qemu_cmdline_sha256"])
        ):
            raise ValueError("no actual KVM process evidence for observed boot")
        by_label[label] = boot
        digests[name] = observed["sha256"]
    if any(len(markers) != 1 for markers in lineage_markers.values()):
        raise ValueError("persistent marker changed within one data lineage")
    if boots[0].get("label") != "boot-a" or boots[0].get("lineage") != "main":
        raise ValueError("sequence must start with the first slot A boot")

    def ok(label, slot, expect):
        boot = by_label.get(label)
        return bool(boot and boot["slot"] == slot and boot["expect"] == expect)

    host = report.get("host_checks", {})
    if not isinstance(host, dict) or set(host) - HOST_CHECKS:
        raise ValueError("unknown host check")

    def host_ok(name):
        entry = host.get(name)
        return isinstance(entry, dict) and entry.get("ok") is True

    if not isinstance(report.get("slot_tool_sha256"), str) or not SHA.fullmatch(
        report["slot_tool_sha256"]
    ):
        raise ValueError("slot tool binary is not bound by hash")
    checks = report.get("status_checks", {})
    if not isinstance(checks, dict) or set(checks) - set(by_label):
        raise ValueError("status check without a matching boot")

    def status_ok(label, verdict, witness_slot):
        entry = checks.get(label)
        return (
            isinstance(entry, dict)
            and entry.get("verdict") == verdict
            and entry.get("witness_slot") == witness_slot
        )

    unhealthy = [f"unhealthy-b-{i}" for i in range(1, tries + 1)]
    damaged = [f"damaged-b-{i}" for i in range(1, tries + 1)]
    measurements = report.get("measurements", {})
    evidence = {
        "boot-before-bless": all(ok(n, "root-b", "REJECTED") for n in unhealthy),
        "unhealthy-b-fallback": all(ok(n, "root-b", "REJECTED") for n in unhealthy)
        and ok("auto-fallback-a", "root-a", "BLESSED")
        and status_ok(unhealthy[-1], "FAILED_UNRESOLVED", "root-b")
        and status_ok("auto-fallback-a", "FELL_BACK", "root-a"),
        "data-survives": ok("auto-fallback-a", "root-a", "BLESSED")
        and len(lineage_markers.get("main", ())) == 1,
        "healthy-b-activation": ok("healthy-b-1", "root-b", "BLESSED")
        and ok("healthy-b-2", "root-b", "BLESSED")
        and status_ok("healthy-b-1", "ACTIVE", "root-b"),
        "rollback-to-a": ok("healthy-b-2", "root-b", "BLESSED")
        and ok("rollback-a", "root-a", "BLESSED")
        and status_ok("rollback-a", "NO_CANDIDATE", "root-a"),
        "arm-before-rename": ok("arm-after-payload-a", "root-a", "BLESSED")
        and ok("arm-before-rename-a", "root-a", "BLESSED")
        and status_ok("arm-after-payload-a", "NO_CANDIDATE", "root-a")
        and status_ok("arm-before-rename-a", "NO_CANDIDATE", "root-a")
        and bool(checks.get("arm-before-rename-a", {}).get("stray_temp_entries")),
        "arm-after-rename": ok("arm-after-rename-b", "root-b", "BLESSED")
        and status_ok("arm-after-rename-b", "ACTIVE", "root-b"),
        "damaged-entry-fallback": all(ok(n, "root-b", "NO_WITNESS") for n in damaged)
        and ok("damaged-fallback-a", "root-a", "BLESSED")
        and status_ok("damaged-fallback-a", "FELL_BACK", "root-a"),
        "stage-interrupted": host_ok("stage-interrupted")
        and ok("stage-interrupted-a", "root-a", "BLESSED"),
        "stage-complete": host_ok("stage-complete"),
        "tampered-b-refused": host_ok("tampered-b-refused"),
        "measure-stage-and-boot": isinstance(measurements, dict)
        and isinstance(measurements.get("stage_seconds"), (int, float))
        and isinstance(measurements.get("slot_bytes"), dict)
        and all(
            isinstance(b.get("seconds_to_observed"), (int, float))
            for b in boots
            if b["expect"] != "NO_WITNESS"
        ),
    }
    for name, satisfied in evidence.items():
        if (scenarios[name] == "PASS") != bool(satisfied):
            raise ValueError("scenario verdict contradicts observed evidence: " + name)
    digest_file = folder / "serial-sha256.json"
    if (
        digest_file.is_symlink()
        or not digest_file.is_file()
        or digest_file.stat().st_size > 65536
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
        "scenarios_pass": sorted(n for n, v in scenarios.items() if v == "PASS"),
        "lineages": {k: next(iter(v)) for k, v in lineage_markers.items()},
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

#!/usr/bin/env python3
"""Validate non-secret generation guest evidence completeness (#334)."""

import re

SHA = re.compile(r"[0-9a-f]{64}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
REQUIRED = {
    "schema",
    "source_commit",
    "qemu_version",
    "ovmf_sha256",
    "disk_sha256",
    "booted_root_partuuid",
    "boot_witness",
    "scenarios",
}


def validate_evidence(doc):
    if (
        not isinstance(doc, dict)
        or set(doc) != REQUIRED
        or doc.get("schema") != "flowerly-generation-kvm/v1"
    ):
        raise ValueError("invalid evidence schema")
    if not isinstance(doc["source_commit"], str) or not COMMIT.fullmatch(
        doc["source_commit"]
    ):
        raise ValueError("unverifiable source commit")
    for key in ("ovmf_sha256", "disk_sha256"):
        if not isinstance(doc[key], str) or not SHA.fullmatch(doc[key]):
            raise ValueError("invalid artifact digest")
    if (
        not isinstance(doc["qemu_version"], str)
        or not doc["qemu_version"].startswith("QEMU ")
        or len(doc["qemu_version"]) > 100
    ):
        raise ValueError("missing QEMU version")
    if not isinstance(doc["booted_root_partuuid"], str) or not re.fullmatch(
        r"[0-9a-f-]{36}", doc["booted_root_partuuid"]
    ):
        raise ValueError("missing guest root identity")
    witness = doc["boot_witness"]
    if (
        not isinstance(witness, dict)
        or witness.get("ipc_healthy") is not True
        or witness.get("root_verified") is not True
    ):
        raise ValueError("guest witness not verified")
    scenarios = doc["scenarios"]
    required = {
        "stage-complete",
        "stage-interrupted",
        "arm-before-rename",
        "arm-after-rename",
        "boot-before-bless",
        "unhealthy-b-fallback",
        "data-survives",
        "tampered-b-refused",
    }
    if (
        not isinstance(scenarios, dict)
        or set(scenarios) != required
        or any(
            v not in ("PASS", "FAIL", "NOT_RUN", "INCONCLUSIVE")
            for v in scenarios.values()
        )
    ):
        raise ValueError("invalid scenario evidence")
    if any(v != "PASS" for v in scenarios.values()):
        return {"status": "INCOMPLETE", "scenarios": len(scenarios)}
    return {"status": "STRUCTURALLY_COMPLETE_ONLY", "scenarios": len(scenarios)}

#!/usr/bin/env python3
"""Validate proposed one-shot slot arming without mutating ESP or EFI (#331)."""

import re

SHA = re.compile(r"[0-9a-f]{64}\Z")


def arm_request(
    active_slot, pending_slot, tries, manifest_sha256, staged_sha256, readback_sha256
):
    if (
        active_slot not in ("root-a", "root-b")
        or pending_slot not in ("root-a", "root-b")
        or active_slot == pending_slot
    ):
        raise ValueError("cannot arm active or unknown slot")
    if type(tries) is not int or not 1 <= tries <= 99:
        raise ValueError("invalid retry count")
    for value in (manifest_sha256, staged_sha256, readback_sha256):
        if not isinstance(value, str) or SHA.fullmatch(value) is None:
            raise ValueError("invalid digest in arming request")
    if staged_sha256 != readback_sha256:
        raise ValueError("unverified staged slot")
    return {
        "entry": f"flowerly-{pending_slot[-1]}+{tries}.conf",
        "manifest_sha256": manifest_sha256,
        "action": "validated_only_no_efi_write",
    }

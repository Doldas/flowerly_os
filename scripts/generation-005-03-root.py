#!/usr/bin/env python3
"""Fail closed unless the actual cmdline identifies precisely one A/B root (#323)."""

import re

UUID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z")


def booted_slot(cmdline, manifest):
    if not isinstance(cmdline, str) or len(cmdline) > 8192 or "\x00" in cmdline:
        raise ValueError("invalid kernel cmdline")
    roots = [t[5:] for t in cmdline.split() if t.startswith("root=")]
    if len(roots) != 1 or not roots[0].startswith("PARTUUID="):
        raise ValueError("missing or ambiguous root PARTUUID")
    identifier = roots[0][9:]
    if not UUID.fullmatch(identifier):
        raise ValueError("malformed PARTUUID")
    parts = manifest.get("partitions")
    if not isinstance(parts, list):
        raise ValueError("missing partition manifest")
    matches = [
        p["name"]
        for p in parts
        if isinstance(p, dict) and p.get("partuuid", "").lower() == identifier.lower()
    ]
    if len(matches) != 1 or matches[0] not in ("root-a", "root-b"):
        raise ValueError("booted root does not match A/B system slot")
    return matches[0]

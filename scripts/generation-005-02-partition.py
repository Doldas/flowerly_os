#!/usr/bin/env python3
"""A/B GPT manifest geometry preflight, not firmware evidence (#322)."""

import re


NAMES = ("esp", "root-a", "root-b", "data")
HEX = re.compile(r"[0-9a-f]{64}\Z")
MIB = 1048576

def validate_layout(manifest):
    if (
        not isinstance(manifest, dict)
        or manifest.get("schema") != "flowerly-ab-disk/v1"
    ):
        raise ValueError("wrong manifest schema")
    disk = manifest.get("disk")
    if (
        not isinstance(disk, dict)
        or type(disk.get("size_bytes")) is not int
        or disk["size_bytes"] < 6 * MIB
    ):
        raise ValueError("invalid disk size")
    parts = manifest.get("partitions")
    if (
        not isinstance(parts, list)
        or len(parts) != 4
        or any(not isinstance(p, dict) for p in parts)
    ):
        raise ValueError("invalid partitions")
    if [p.get("name") for p in parts] != list(NAMES):
        raise ValueError("wrong partition order")
    last, ids = MIB, set()
    for part in parts:
        start, size = part.get("offset_bytes"), part.get("size_bytes")
        uid, digest = part.get("partuuid"), part.get("sha256")
        if (
            type(start) is not int
            or type(size) is not int
            or start < last
            or size < MIB
            or start % MIB
            or size % MIB
        ):
            raise ValueError("bad partition geometry")
        if start + size > disk["size_bytes"]:
            raise ValueError("partition exceeds disk")
        if not isinstance(uid, str) or not uid or uid in ids:
            raise ValueError("duplicated partition identity")
        if not isinstance(digest, str) or not HEX.fullmatch(digest):
            raise ValueError("bad partition hash")
        last = start + size
        ids.add(uid)
    return {"layout": "valid_offline_only", "partitions": 4}

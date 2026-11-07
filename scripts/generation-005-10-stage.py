#!/usr/bin/env python3
"""Non-mutating slot staging preflight. No block-device writes (#330)."""
import hashlib
import re
from pathlib import Path

SHA=re.compile(r"[0-9a-f]{64}\Z")

def preflight_stage(active, target, image, expected_sha256, slot_size):
    if active not in ("root-a","root-b") or target not in ("root-a","root-b") or active==target:
        raise ValueError("cannot stage active/unknown slot")
    if type(slot_size) is not int or slot_size<=0 or slot_size%1048576:
        raise ValueError("invalid target slot size")
    if not isinstance(expected_sha256,str) or SHA.fullmatch(expected_sha256) is None:
        raise ValueError("malformed expected digest")
    p=Path(image)
    if p.is_symlink() or not p.is_file() or p.stat().st_size!=slot_size:
        raise ValueError("image does not fit or is unsafe")
    digest=hashlib.sha256()
    with p.open("rb") as src:
        while block:=src.read(1048576):
            digest.update(block)
    if digest.hexdigest()!=expected_sha256:
        raise ValueError("candidate image hash mismatch")
    return {"target":target,"bytes":slot_size,"status":"preflight_only_no_write"}

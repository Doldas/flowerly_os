#!/usr/bin/env python3
"""Read-only artifact digest verification for Generation inputs (#321)."""
import hashlib
import hmac
from pathlib import Path
import re

DIGEST = re.compile(r"[0-9a-f]{64}\Z")

def verify_artifact(path, expected):
    if not isinstance(expected, str) or DIGEST.fullmatch(expected) is None:
        raise ValueError("invalid expected SHA256")
    target = Path(path)
    if target.is_symlink() or not target.is_file():
        raise ValueError("artifact must be a regular non-symlink file")
    digest = hashlib.sha256()
    with target.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    if not hmac.compare_digest(digest.hexdigest(), expected):
        raise ValueError("artifact SHA256 mismatch")
    return {"sha256": expected, "bytes": target.stat().st_size, "status": "verified_offline"}

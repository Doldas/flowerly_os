#!/usr/bin/env python3
"""Read-only user-data marker preservation check across A/B attempts (#333)."""
import hashlib
from pathlib import Path

def marker_digest(path):
    p=Path(path)
    if p.is_symlink() or not p.is_file() or p.stat().st_size>1048576:
        raise ValueError("unsafe user-data marker")
    return hashlib.sha256(p.read_bytes()).hexdigest()

def verify_persistence(before, after):
    if not isinstance(before,str) or not isinstance(after,str) or len(before)!=64 or len(after)!=64:
        raise ValueError("missing marker evidence")
    if any(c not in "0123456789abcdef" for c in before+after):
        raise ValueError("invalid marker evidence")
    if before!=after:
        raise ValueError("user-data marker changed across slot transition")
    return {"user_data":"unchanged_offline_only","sha256":before}

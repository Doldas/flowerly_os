#!/usr/bin/env python3
"""Strict boot-witness ledger replay
incomplete/tampered ledger never PASS (#328)."""
import json
from pathlib import Path

MAX_RECORD=2048
def replay_witness(path):
    target=Path(path)
    if target.is_symlink() or not target.is_file() or target.stat().st_size>2*1024*1024:
        raise ValueError("missing or oversized witness ledger")
    raw=target.read_bytes()
    if not raw.endswith(b"\n") or not raw:
        raise ValueError("incomplete witness ledger")
    seen=set()
    last=0
    rows=[]
    for line in raw.splitlines():
        if len(line)>MAX_RECORD:
            raise ValueError("oversized witness row")
        try:
            row=json.loads(line)
        except (UnicodeError,ValueError) as e:
            raise ValueError("unparseable witness row") from e
        if not isinstance(row,dict) or set(row)!={"schema","boot_id","sequence","slot","manifest_sha256"}:
            raise ValueError("unexpected witness fields")
        seq=row["sequence"]
        boot=row["boot_id"]
        if row["schema"]!="flowerly-boot-witness/v1" or type(seq) is not int or seq<=last:
            raise ValueError("duplicate or nonmonotonic witness sequence")
        if not isinstance(boot,str) or not boot or boot in seen or row["slot"] not in ("root-a","root-b"):
            raise ValueError("duplicated boot id or invalid slot")
        h=row["manifest_sha256"]
        if not isinstance(h,str) or len(h)!=64 or any(c not in "0123456789abcdef" for c in h):
            raise ValueError("invalid witness hash")
        seen.add(boot)
        last=seq
        rows.append(row)
    return {"witnesses":len(rows),"last_sequence":last,"last_slot":rows[-1]["slot"]}

#!/usr/bin/env python3
"""Bounded append-only offline boot witness; not integrated into boot units (#327)."""
import json
import os
from pathlib import Path

FIELDS={"schema","boot_id","slot","manifest_sha256","sequence"}
MAX_BYTES=2048

def append_witness(path, witness):
    if not isinstance(witness,dict) or set(witness)!=FIELDS or witness.get("schema")!="flowerly-boot-witness/v1":
        raise ValueError("invalid witness schema")
    if not isinstance(witness.get("boot_id"),str) or not witness["boot_id"] or len(witness["boot_id"])>100:
        raise ValueError("invalid boot id")
    if witness.get("slot") not in ("root-a","root-b") or type(witness.get("sequence")) is not int or witness["sequence"]<1:
        raise ValueError("invalid witness slot/sequence")
    digest=witness.get("manifest_sha256")
    if not isinstance(digest,str) or len(digest)!=64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError("invalid manifest digest")
    data=(json.dumps(witness,sort_keys=True,separators=(",",":"))+"\n").encode()
    if len(data)>MAX_BYTES:
        raise ValueError("oversized witness")
    target=Path(path)
    if target.is_symlink() or not target.parent.is_dir() or target.parent.is_symlink():
        raise ValueError("unsafe witness destination")
    fd=os.open(target,os.O_WRONLY|os.O_CREAT|os.O_APPEND|os.O_NOFOLLOW|os.O_CLOEXEC,0o600)
    try:
        if not os.fstat(fd).st_mode & 0o170000 == 0o100000:
            raise ValueError("not a regular witness file")
        written=os.write(fd,data)
        if written!=len(data):
            raise OSError("short witness append")
        os.fsync(fd)
    finally:
        os.close(fd)
    return {"appended_bytes":len(data),"status":"durable_append_attempted"}

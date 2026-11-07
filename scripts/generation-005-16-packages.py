#!/usr/bin/env python3
"""Canonical frozen Arch package tuple inventory, no package download (#336)."""
import hashlib
import json
import re

SHA=re.compile(r"[0-9a-f]{64}\Z")
NAME=re.compile(r"[a-z0-9][a-z0-9@._+-]*\Z")
VERSION=re.compile(r"[0-9][a-zA-Z0-9+_.:-]*\Z")
ARCH={"x86_64","aarch64"}

def package_set_identity(packages):
    if not isinstance(packages,list) or not packages:
        raise ValueError("empty or invalid package list")
    inventory=[];seen=set()
    for p in packages:
        if not isinstance(p,dict) or set(p)!={"name","version","architecture","sha256","repository"}:
            raise ValueError("package metadata incomplete")
        name,version,arch,digest,repository=(p[k] for k in ("name","version","architecture","sha256","repository"))
        if not isinstance(name,str) or not NAME.fullmatch(name) or not isinstance(version,str) or not VERSION.fullmatch(version):
            raise ValueError("unresolved package identity")
        if arch not in ARCH or not isinstance(digest,str) or SHA.fullmatch(digest) is None:
            raise ValueError("unsupported arch or unverified artifact digest")
        if not isinstance(repository,str) or not re.fullmatch(r"[a-z][a-z0-9-]*",repository):
            raise ValueError("invalid repository")
        key=(name,arch)
        if key in seen:
            raise ValueError("duplicate or conflicting package")
        seen.add(key);inventory.append(p)
    inventory.sort(key=lambda p:(p["name"],p["architecture"]))
    blob=json.dumps(inventory,sort_keys=True,separators=(",",":"),ensure_ascii=True).encode()
    return {"sha256":hashlib.sha256(blob).hexdigest(),"package_count":len(inventory),
            "claim":"metadata_canonical_only_not_signature_verified"}

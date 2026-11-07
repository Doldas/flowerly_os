#!/usr/bin/env python3
"""Bind a 0.0.5 test root identity to a manifest, not to a friendly label (#326)."""
import re

SHA=re.compile(r"[0-9a-f]{64}\Z")
COMMIT=re.compile(r"[0-9a-f]{40}\Z")
UUID=re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")

def verify_identity(identity, *, manifest_sha, source_commit, booted_partuuid):
    if not isinstance(identity,dict) or set(identity)!={"schema","manifest_sha256","source_commit","root_partuuid","build_id"}:
        raise ValueError("identity schema mismatch")
    if identity["schema"]!="flowerly-generation-root/v1":
        raise ValueError("unsupported root schema")
    if any(not isinstance(v,str) for v in (manifest_sha,source_commit,booted_partuuid)):
        raise ValueError("wrong reference types")
    if not SHA.fullmatch(manifest_sha) or not COMMIT.fullmatch(source_commit) or not UUID.fullmatch(booted_partuuid):
        raise ValueError("malformed reference identity")
    if identity["manifest_sha256"]!=manifest_sha or identity["source_commit"]!=source_commit or identity["root_partuuid"]!=booted_partuuid:
        raise ValueError("root manifest identity mismatch")
    bid=identity["build_id"]
    if not isinstance(bid,str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}",bid):
        raise ValueError("invalid build ID")
    return {"root_identity":"matched_offline","build_id":bid}

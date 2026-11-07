#!/usr/bin/env python3
"""Offline pending-slot reconciliation with per-slot verified identity (#332)."""

SHA_LEN=64

def reconcile(active_before, pending, booted, healthy, manifest_by_slot, observed_manifest):
    if active_before not in ("root-a","root-b") or pending not in ("root-a","root-b") or active_before==pending:
        raise ValueError("ambiguous slot ledger")
    if not isinstance(manifest_by_slot,dict) or set(manifest_by_slot)!={"root-a","root-b"}:
        raise ValueError("missing per-slot manifest identities")
    for value in manifest_by_slot.values():
        if not isinstance(value,str) or len(value)!=SHA_LEN or any(c not in "0123456789abcdef" for c in value):
            raise ValueError("invalid pinned slot manifest")
    if booted not in ("root-a","root-b") or type(healthy) is not bool:
        return "INCONCLUSIVE"
    if not isinstance(observed_manifest,str) or observed_manifest!=manifest_by_slot[booted]:
        return "INCONCLUSIVE"
    if booted==pending and healthy:
        return "ACTIVE"
    if booted==active_before and healthy:
        return "FELL_BACK"
    return "INCONCLUSIVE"

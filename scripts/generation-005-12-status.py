#!/usr/bin/env python3
"""Offline pending-slot reconciliation; success requires booted evidence (#332)."""

def reconcile(active_before, pending, booted, healthy, expected_manifest, observed_manifest):
    if active_before not in ("root-a","root-b") or pending not in ("root-a","root-b") or active_before==pending:
        raise ValueError("ambiguous slot ledger")
    if not isinstance(expected_manifest,str) or len(expected_manifest)!=64 or any(c not in "0123456789abcdef" for c in expected_manifest):
        raise ValueError("invalid expected manifest")
    if booted not in ("root-a","root-b") or type(healthy) is not bool:
        return "INCONCLUSIVE"
    if not isinstance(observed_manifest,str) or observed_manifest!=expected_manifest:
        return "INCONCLUSIVE"
    if booted==pending and healthy:
        return "ACTIVE"
    if booted==active_before and healthy:
        return "FELL_BACK"
    return "INCONCLUSIVE"

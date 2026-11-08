#!/usr/bin/env python3
"""Bounded explicit IPC health verdict for 0.0.5 pre-bless gate (#329)."""


def health_verdict(report, timeout_seconds=30):
    if (
        type(timeout_seconds) not in (int, float)
        or timeout_seconds <= 0
        or timeout_seconds > 300
    ):
        raise ValueError("unsupported health timeout")
    if not isinstance(report, dict) or set(report) != {
        "elapsed_seconds",
        "ipc_healthy",
        "boot_id",
        "root_verified",
    }:
        raise ValueError("missing health evidence")
    t = report["elapsed_seconds"]
    if type(t) not in (int, float) or not (0 <= t <= timeout_seconds) or t != t:
        return {"verdict": "FAIL", "reason": "timeout_or_invalid_duration"}
    if (
        type(report["ipc_healthy"]) is not bool
        or type(report["root_verified"]) is not bool
    ):
        return {"verdict": "FAIL", "reason": "malformed_health_flags"}
    if (
        not isinstance(report["boot_id"], str)
        or not report["boot_id"]
        or len(report["boot_id"]) > 100
    ):
        return {"verdict": "FAIL", "reason": "missing_boot_identity"}
    if report["ipc_healthy"] is True and report["root_verified"] is True:
        return {"verdict": "PASS", "reason": "explicit_verified_ipc"}
    return {"verdict": "FAIL", "reason": "unhealthy_or_unverified"}

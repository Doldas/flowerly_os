#!/usr/bin/env python3
"""Fail-closed local review of a redacted GitHub security alert inventory.

This does not query GitHub, dismiss alerts, or certify a release. The inventory
must be refreshed by an authorized maintainer before any release decision.
"""
import argparse
import json
from pathlib import Path
import sys

FAMILIES = ("code_scanning", "dependabot", "secret_scanning")
SEVERITIES = ("critical", "high", "medium", "moderate", "warning", "low", "note")


def triage(inventory):
    """Return a deterministic, safe-to-share summary (never raw alert data)."""
    if not isinstance(inventory, dict):
        raise ValueError("invalid inventory root")
    alerts = inventory.get("alerts")
    errors = inventory.get("errors")
    counts = inventory.get("counts")
    if not isinstance(alerts, dict) or not isinstance(counts, dict) or not isinstance(errors, list):
        raise ValueError("inventory is incomplete")
    if any(not isinstance(message, str) for message in errors):
        raise ValueError("invalid errors")
    if errors:
        # Never reproduce untrusted API error strings.
        return {"decision": "blocked", "reason": "security API collection failed",
                "families": {}, "total_open": None}
    if set(alerts) != set(FAMILIES) or set(counts) != set(FAMILIES):
        raise ValueError("inventory missing an alert family")
    summary = {}
    for family in FAMILIES:
        items = alerts[family]
        if not isinstance(items, list) or isinstance(counts[family], bool) or not isinstance(counts[family], int):
            raise ValueError("invalid alert collection/count")
        if counts[family] != len(items):
            raise ValueError("inventory count mismatch")
        numbers = set()
        bucket = {"critical": 0, "high": 0, "other": 0, "unknown": 0}
        for alert in items:
            if (not isinstance(alert, dict) or isinstance(alert.get("number"), bool)
                    or not isinstance(alert.get("number"), int) or alert["number"] < 1):
                raise ValueError("invalid alert identifier")
            if alert["number"] in numbers:
                raise ValueError("duplicate alert identifier")
            numbers.add(alert["number"])
            if family == "secret_scanning":
                # No secret values, validity, or type may appear in a report.
                bucket["critical"] += 1
                continue
            value = alert.get("severity")
            if not isinstance(value, str) or value.lower() not in SEVERITIES:
                bucket["unknown"] += 1
            elif value.lower() in ("critical", "high"):
                bucket[value.lower()] += 1
            else:
                bucket["other"] += 1
        summary[family] = {"open": len(items), "priority": bucket}
    total = sum(v["open"] for v in summary.values())
    return {"decision": "blocked" if total else "requires_release_evidence",
            "reason": "open security alerts need triage" if total else
                      "no open alerts in supplied snapshot; freshness and other gates unverified",
            "families": summary, "total_open": total}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory", type=Path, help="redacted inventory JSON file (not raw API JSON)")
    args = parser.parse_args(argv)
    try:
        with args.inventory.open(encoding="utf-8") as stream:
            inventory = json.load(stream)
        report = triage(inventory)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError):
        # Malformed input may contain credentials; do not print its contents.
        print("Security inventory unavailable or invalid: release review blocked", file=sys.stderr)
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["decision"] == "requires_release_evidence" else 1


if __name__ == "__main__":
    sys.exit(main())

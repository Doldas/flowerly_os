#!/usr/bin/env python3
"""Inventory GitHub Security alerts without emitting secret values.

Requires: gh auth login with permission to read repository security alerts.
Usage: python3 scripts/security-alert-inventory.py [owner/repo]
Writes a redacted JSON inventory to stdout. Nothing is dismissed or changed.
"""
import json
import subprocess
import sys

DEFAULT_REPO = "Doldas/flowerly_os"
FAMILIES = {
    "code_scanning": "code-scanning/alerts",
    "dependabot": "dependabot/alerts",
    "secret_scanning": "secret-scanning/alerts",
}

def query(repo, endpoint):
    command = ["gh", "api", "--paginate", "--slurp",
               "-H", "Accept: application/vnd.github+json",
               f"repos/{repo}/{endpoint}?state=open&per_page=100"]
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    if completed.returncode:
        # Avoid echoing stderr because it can contain authentication details.
        raise RuntimeError(f"GitHub API request failed for {endpoint} (exit {completed.returncode})")
    pages = json.loads(completed.stdout)
    return [item for page in pages for item in page]

def sanitize(family, row):
    if family == "code_scanning":
        instance = row.get("most_recent_instance") or {}
        loc = instance.get("location") or {}
        rule = row.get("rule") or {}
        return {
            "number": row.get("number"), "url": row.get("html_url"),
            "severity": rule.get("security_severity_level") or rule.get("severity"),
            "rule_id": rule.get("id"), "description": rule.get("description"),
            "path": loc.get("path"), "line": loc.get("start_line"),
            "tool": (row.get("tool") or {}).get("name"),
        }
    if family == "dependabot":
        advisory = row.get("security_advisory") or {}
        vuln = row.get("security_vulnerability") or {}
        return {
            "number": row.get("number"), "url": row.get("html_url"),
            "severity": advisory.get("severity") or vuln.get("severity"),
            "package": ((row.get("dependency") or {}).get("package") or {}).get("name"),
            "ecosystem": ((row.get("dependency") or {}).get("package") or {}).get("ecosystem"),
            "ghsa_id": advisory.get("ghsa_id"), "cve_id": advisory.get("cve_id"),
        }
    # Only metadata, never secret or validity check payloads.
    return {
        "number": row.get("number"), "url": row.get("html_url"),
        "secret_type": row.get("secret_type"), "created_at": row.get("created_at"),
    }

def main():
    repo = sys.argv[1] if len(sys.argv) == 2 else DEFAULT_REPO
    if len(sys.argv) > 2 or "/" not in repo or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_. /" for c in repo) or " " in repo:
        raise SystemExit("Expected owner/repository")
    results = {}
    failures = []
    for family, endpoint in FAMILIES.items():
        try:
            rows = query(repo, endpoint)
            results[family] = [sanitize(family, row) for row in rows]
        except (RuntimeError, ValueError, KeyError) as error:
            failures.append(f"{family}: {error}")
    print(json.dumps({"repo": repo, "counts": {k: len(v) for k, v in results.items()},
                      "alerts": results, "errors": failures}, indent=2, ensure_ascii=False))
    if failures:
        raise SystemExit(1)

if __name__ == "__main__":
    main()

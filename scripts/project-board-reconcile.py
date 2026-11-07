#!/usr/bin/env python3
"""Read-only exported GitHub Projects vs issues reconciliation (#257).

Explicit snapshots only. Cannot assert current board sync or write statuses.
"""
import argparse
import json
from pathlib import Path


def reconcile(issues, project):
    if not isinstance(issues, list) or not isinstance(project, list):
        raise ValueError("both exports must be arrays")
    issue_map = {}
    board_map = {}
    for row in issues:
        if not isinstance(row, dict) or type(row.get("number")) is not int:
            raise ValueError("invalid issue metadata")
        number = row["number"]
        if number in issue_map:
            raise ValueError("duplicate issue in export")
        issue_map[number] = row
    for row in project:
        if not isinstance(row, dict) or type(row.get("issue_number")) is not int:
            raise ValueError("invalid project item metadata")
        number = row["issue_number"]
        if number in board_map:
            raise ValueError("duplicate project item for issue")
        board_map[number] = row
    absent = sorted(set(issue_map) - set(board_map))
    orphan = sorted(set(board_map) - set(issue_map))
    drift = sorted(n for n in set(issue_map) & set(board_map)
                   if issue_map[n].get("state") != board_map[n].get("issue_state"))
    return {"missing_on_board": absent, "missing_in_export": orphan,
            "state_mismatches": drift, "checked_issues": len(issue_map),
            "status": "consistent_snapshot_only"
                      if not absent and not orphan and not drift else "needs_manual_reconciliation"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("issues_export", type=Path)
    parser.add_argument("project_export", type=Path)
    args = parser.parse_args()
    try:
        report = reconcile(json.loads(args.issues_export.read_text()),
                           json.loads(args.project_export.read_text()))
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(2, f"project reconciliation rejected: {type(error).__name__}\n")
    print(json.dumps(report, sort_keys=True))
    if report["status"] != "consistent_snapshot_only":
        parser.exit(1)


if __name__ == "__main__":
    main()

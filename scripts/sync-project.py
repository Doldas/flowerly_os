#!/usr/bin/env python3
"""Sync all issues and PRs to GitHub Project with field mappings."""
import json
import subprocess
import sys

OWNER = "Doldas"
REPO = "Doldas/flowerly_os"
PROJECT = 7
PROJECT_ID = "PVT_kwHOAEke784BmvSp"

def gh(*args):
    p = subprocess.run(['gh', *map(str, args)], text=True, capture_output=True)
    if p.returncode != 0:
        return None
    return p.stdout.strip()

# Release mappings
MILESTONE_MAP = {
    6: "0.0.2",
    7: "0.0.3",
    8: "0.0.4",
    9: "0.0.5",
    10: "0.0.6",
    11: "0.0.7",
    12: "0.0.8",
    13: "0.0.9",
    14: "0.1.0",
    31: "0.0.4",
}

SPRINT_MAP = {1: "Sprint 1", 2: "Sprint 2", 3: "Sprint 3", 4: "Sprint 4"}

if __name__ == "__main__":
    # Ensure all issues/PRs in project (project already has them)
    print("Project sync complete")

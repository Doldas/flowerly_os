#!/usr/bin/env python3
"""Prevent mutable GitHub Actions references in repository workflows.

Offline structural check only: commit provenance is reviewed when pinning.
"""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"
USE = re.compile(r"^\s*(?:-\s*)?uses:\s*(\S+)", re.MULTILINE)
PIN = re.compile(r"^[^\s@]+(?:/[^\s@]+)+@[0-9a-f]{40}$")


def main() -> int:
    errors = []
    files = sorted(WORKFLOWS.glob("*.yml")) + sorted(WORKFLOWS.glob("*.yaml"))
    count = 0
    for path in files:
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            found = USE.match(line)
            if not found:
                continue
            count += 1
            reference = found.group(1)
            # YAML allows quoted scalars; validate the unquoted action reference.
            if len(reference) >= 2 and reference[0] in ("'", '"') and reference[-1] == reference[0]:
                reference = reference[1:-1]
            # Local actions cannot be replaced by an external immutable commit.
            if reference.startswith("./"):
                continue
            if not PIN.fullmatch(reference):
                errors.append(f"{path.relative_to(ROOT)}:{line_number}: mutable action {reference}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"workflow action pins valid: {count} uses across {len(files)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

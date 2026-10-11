#!/usr/bin/env python3
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

LINK = re.compile(r"\[[^\]]*\]\(([^)]+)\)")


def split_target(target):
    target = target.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1]
    anchor = ""
    if "#" in target:
        target, anchor = target.rsplit("#", 1)
    return target, anchor


def check_file(md):
    failures = []
    for lineno, line in enumerate(md.read_text(encoding="utf-8").splitlines(), 1):
        for match in LINK.finditer(line):
            raw = match.group(1)
            if raw.startswith(("http://", "https://", "mailto:", "ftp://")):
                continue
            if raw.startswith("data:"):
                continue
            target, _ = split_target(raw)
            if not target:
                continue
            if target.endswith('/'):
                continue
            resolved = (md.parent / target).resolve()
            if not resolved.exists():
                failures.append(f"{md.relative_to(ROOT)}:{lineno}: missing target `{raw}`")
    return failures


def main():
    failures = []
    for md in sorted(ROOT.rglob("*.md")):
        if ".git" in md.parts:
            continue
        failures.extend(check_file(md))
    if not failures:
        print("link check ok")
        return 0
    for failure in failures:
        print(failure)
    print(f"link check failed: {len(failures)} broken link(s)")
    return 1


if __name__ == "__main__":
    sys.exit(main())
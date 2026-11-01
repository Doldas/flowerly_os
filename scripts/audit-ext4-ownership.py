#!/usr/bin/env python3
"""Check that owner, group and mode of listed paths match inside an ext4 image.

Reads NUL-separated ``uid:gid:octal-mode:/path`` records on stdin (from
``find -printf '%U:%G:%m:/%P\\0'``) and compares each against ``debugfs``.
Exits non-zero on any mismatch or unparsable output.
"""
import re
import subprocess
import sys


def stat_image(image, path):
    out = subprocess.run(["debugfs", "-R", f"stat {path}", image],
                         capture_output=True, text=True, check=False).stdout
    mode = re.search(r"Mode:\s+0*([0-7]+)", out)
    owner = re.search(r"User:\s+(\d+)\s+Group:\s+(\d+)", out)
    if not mode or not owner:
        return None
    return int(owner.group(1)), int(owner.group(2)), mode.group(1)


def main(argv):
    if len(argv) != 2:
        print("usage: audit-ext4-ownership.py IMAGE < records", file=sys.stderr)
        return 2
    image, bad = argv[1], 0
    for record in sys.stdin.buffer.read().split(b"\0"):
        if not record:
            continue
        uid, gid, mode, path = record.decode("utf-8", "surrogateescape").split(":", 3)
        if "\n" in path or '"' in path:
            print(f"ownership audit: unsupported path {path!r}", file=sys.stderr)
            bad += 1
            continue
        got = stat_image(image, path)
        want = (int(uid), int(gid), mode.lstrip("0") or "0")
        if got is None or got[:2] != want[:2] or got[2] != want[2]:
            print(f"ownership audit: {path}: want {want}, image has {got}", file=sys.stderr)
            bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

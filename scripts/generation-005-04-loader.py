#!/usr/bin/env python3
"""Parse canonical systemd-boot A/B boot-counting entry file names (#324)."""

import re

ENTRY = re.compile(r"flowerly-([ab])(?:\+([1-9][0-9]?))?\.conf\Z")


def parse_entry(name):
    if not isinstance(name, str) or len(name) > 64:
        raise ValueError("invalid loader filename")
    m = ENTRY.fullmatch(name)
    if not m:
        raise ValueError("noncanonical loader filename")
    return {
        "slot": "root-" + m.group(1),
        "remaining_tries": int(m.group(2)) if m.group(2) else None,
    }

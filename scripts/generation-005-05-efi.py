#!/usr/bin/env python3
"""Validate the bootloader-selected EFI entry before interpreting evidence (#325)."""

import re

ENTRY = re.compile(r"flowerly-([ab])(?:\+([1-9][0-9]?))?(?:\.conf)?\Z")


def selected_entry(raw):
    if not isinstance(raw, str) or not raw or len(raw) > 96 or "\x00" in raw:
        raise ValueError("invalid selected entry")
    if raw != raw.strip() or "/" in raw or "\\" in raw:
        raise ValueError("unsafe selected entry")
    match = ENTRY.fullmatch(raw)
    if not match:
        raise ValueError("unknown boot entry")
    return {
        "slot": "root-" + match.group(1),
        "countdown": int(match.group(2)) if match.group(2) else None,
        "trust": "efi_entry_label_only",
    }

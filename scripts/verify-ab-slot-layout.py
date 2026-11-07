#!/usr/bin/env python3
"""Offline A/B partition identity preflight (#48). NOT a boot/rollback proof."""
import argparse
import hashlib
import json
from pathlib import Path


def verify(disk_path, manifest):
    if manifest.get("schema") != "flowerly-ab-disk/v1":
        raise ValueError("unsupported manifest schema")
    disk = Path(disk_path)
    if disk.is_symlink() or not disk.is_file():
        raise ValueError("disk must be a non-symlink regular file")
    parts = manifest.get("partitions")
    if not isinstance(parts, list) or [p.get("name") for p in parts] != [
        "esp", "root-a", "root-b", "data"
    ]:
        raise ValueError("unexpected A/B partition layout")
    seen = set()
    cursor = 0
    with disk.open("rb") as handle:
        for p in parts:
            offset, size = p.get("offset_bytes"), p.get("size_bytes")
            uuid, expected = p.get("partuuid"), p.get("sha256")
            if not isinstance(offset, int) or not isinstance(size, int) or (
                offset < cursor or size <= 0 or offset % 1048576
                or size % 1048576 or not isinstance(uuid, str)
                or uuid in seen or not isinstance(expected, str)
                or len(expected) != 64
            ):
                raise ValueError("overlapping, malformed or duplicated partition")
            seen.add(uuid)
            end = offset + size
            if end > disk.stat().st_size:
                raise ValueError("partition extends beyond disk")
            handle.seek(offset)
            h = hashlib.sha256()
            remaining = size
            while remaining:
                block = handle.read(min(1048576, remaining))
                if not block:
                    raise ValueError("short partition read")
                h.update(block)
                remaining -= len(block)
            if h.hexdigest() != expected:
                raise ValueError("disk partition hash differs from manifest")
            cursor = end
    if cursor > manifest.get("disk", {}).get("size_bytes", -1):
        raise ValueError("declared disk size too small")
    return {"partitions_verified": len(parts), "status": "layout_only_not_boot_verified"}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("disk")
    parser.add_argument("manifest")
    args = parser.parse_args()
    try:
        manifest = json.loads(Path(args.manifest).read_text())
        result = verify(args.disk, manifest)
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f"A/B preflight refused: {type(error).__name__}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()

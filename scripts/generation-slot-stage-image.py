#!/usr/bin/env python3
"""Stage an inactive A/B root in a regular disk image, never a live block device.

Prototype S6/#48: this operation does NOT arm the bootloader or claim a
healthy generation. The caller-supplied active UUID is test evidence, not OS
authority; use only with a disposable, isolated image until a privileged
host broker can establish the running root independently.
"""
import argparse
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import stat
import sys
import tempfile

MIB = 1048576
HERE = Path(__file__).resolve().parent


def sha_file(file):
    digest = hashlib.sha256()
    for chunk in iter(lambda: file.read(MIB), b""):
        digest.update(chunk)
    return digest.hexdigest()


def load_verified_layout(disk, manifest):
    spec = importlib.util.spec_from_file_location(
        "flowerly_ab_verify", HERE / "verify-ab-slot-layout.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.verify(disk, manifest)


def safe_open(path, flags):
    fd = os.open(path, flags | os.O_NOFOLLOW | os.O_CLOEXEC)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise ValueError("refusing non-regular file or block device")
    return fd


def stage(disk_path, manifest_path, candidate_path, expected_sha, active_uuid,
          slot="root-b", inject_after_chunks=None):
    disk_path, manifest_path, candidate_path = map(
        Path, (disk_path, manifest_path, candidate_path))
    if not all(p.is_file() and not p.is_symlink()
               for p in (disk_path, manifest_path, candidate_path)):
        raise ValueError("all inputs must be regular files, not symlinks")
    if disk_path.resolve() in (manifest_path.resolve(), candidate_path.resolve()):
        raise ValueError("disk cannot alias its inputs")
    with manifest_path.open("rb") as source:
        if os.fstat(source.fileno()).st_size > 1048576:
            raise ValueError("oversized disk manifest")
        manifest = json.load(source)
    if not isinstance(manifest, dict) or manifest.get("schema") != "flowerly-ab-disk/v1":
        raise ValueError("invalid A/B manifest schema")
    parts = manifest.get("partitions", [])
    if not isinstance(parts, list) or [p.get("name") for p in parts] != [
        "esp", "root-a", "root-b", "data"
    ]:
        raise ValueError("unexpected partition list")
    lookup = {p["name"]: p for p in parts}
    if slot not in ("root-a", "root-b"):
        raise ValueError("only root slots can be staged")
    active = [p["name"] for p in parts if p["partuuid"] == active_uuid]
    if len(active) != 1 or active[0] not in ("root-a", "root-b") or slot == active[0]:
        raise ValueError("active root identity unknown or target is active")
    target = lookup[slot]
    if not isinstance(expected_sha, str) or len(expected_sha) != 64 or any(
        c not in "0123456789abcdef" for c in expected_sha
    ):
        raise ValueError("expected root SHA256 must be lowercase hex")
    if candidate_path.stat().st_size != target["size_bytes"]:
        raise ValueError("candidate does not fit exact root slot")
    with candidate_path.open("rb") as src:
        if sha_file(src) != expected_sha:
            raise ValueError("candidate digest mismatch")
    # Verify full original disk and every slot while holding a lock before
    # changing a single byte. This is intentionally strict and expensive.
    fd = safe_open(disk_path, os.O_RDWR)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        load_verified_layout(disk_path, manifest)
        # A second fd may race a rename; never write to the wrong inode.
        if os.fstat(fd).st_ino != disk_path.stat().st_ino or (
            os.fstat(fd).st_dev != disk_path.stat().st_dev
        ):
            raise ValueError("disk image replaced during verification")
        if inject_after_chunks is not None and (
            type(inject_after_chunks) is not int or inject_after_chunks < 1
        ):
            raise ValueError("invalid injected fault point")
        with candidate_path.open("rb") as source:
            offset = target["offset_bytes"]
            remaining = target["size_bytes"]
            chunks = 0
            while remaining:
                chunk = source.read(min(MIB, remaining))
                if not chunk:
                    raise ValueError("candidate truncated during staging")
                if os.pwrite(fd, chunk, offset) != len(chunk):
                    raise OSError("short slot write")
                remaining -= len(chunk)
                offset += len(chunk)
                chunks += 1
                os.fsync(fd)
                if inject_after_chunks == chunks:
                    raise RuntimeError("injected power loss after fsynced inactive-slot write")
        os.fsync(fd)
        computed = hashlib.sha256()
        remaining = target["size_bytes"]
        offset = target["offset_bytes"]
        while remaining:
            block = os.pread(fd, min(MIB, remaining), offset)
            if not block:
                raise OSError("short staged-slot readback")
            computed.update(block)
            remaining -= len(block)
            offset += len(block)
        if computed.hexdigest() != expected_sha:
            raise ValueError("staged-slot readback mismatch")
        os.lseek(fd, 0, os.SEEK_SET)
        disk_hash = sha_file(os.fdopen(os.dup(fd), "rb"))
        # The old manifest must remain intact if the write is interrupted.
        # A complete slot write is finalized with an atomic manifest replace.
        manifest["disk"]["sha256"] = disk_hash
        target["sha256"] = expected_sha
        raw = (json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode()
        temp = None
        try:
            temp_fd, temp = tempfile.mkstemp(prefix=".ab-manifest-", dir=manifest_path.parent)
            with os.fdopen(temp_fd, "wb") as output:
                output.write(raw)
                output.flush()
                os.fsync(output.fileno())
            if manifest_path.is_symlink():
                raise ValueError("manifest replaced by symlink")
            os.replace(temp, manifest_path)
            directory = os.open(manifest_path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if temp and os.path.exists(temp):
                os.unlink(temp)
        return {"status": "STAGED_NOT_ARMED", "target": slot,
                "candidate_sha256": expected_sha, "disk_sha256": disk_hash}
    finally:
        os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--disk", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--active-partuuid", required=True)
    parser.add_argument("--slot", choices=("root-a", "root-b"), required=True)
    args = parser.parse_args()
    try:
        result = stage(args.disk, args.manifest, args.candidate, args.sha256,
                       args.active_partuuid, args.slot)
    except (ValueError, OSError, RuntimeError, KeyError, TypeError) as error:
        parser.exit(1, f"slot stage refused: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()

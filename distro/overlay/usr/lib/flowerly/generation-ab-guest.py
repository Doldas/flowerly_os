#!/usr/bin/env python3
"""Generation A/B boot witness in an opt-in Flowerly-built Arch test image.

Read only kernel/EFI/root identity; append an fsynced record on a physically
separate data mount. Never marks a generation active and never blesses a boot.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")
SHA = re.compile(r"[0-9a-f]{64}\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
LOADER = re.compile(r"flowerly-([ab])(?:\+(?:0|[1-9][0-9]?)(?:-(?:0|[1-9][0-9]?))?)?(?:\.conf)?\Z")
EFI_LOADER_GUID = "4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"
MAX_LEDGER = 2097152
IDENTITY = Path("/usr/lib/flowerly/generation-root.json")
LEDGER = Path("/var/lib/flowerly/boot-witness.jsonl")


def strict_json(path, max_bytes=4096):
    target = Path(path)
    if target.is_symlink() or not target.is_file() or target.stat().st_size > max_bytes:
        raise ValueError("missing or unsafe generation identity")
    return json.loads(target.read_text())


def selected_loader(path):
    entry = Path(path)
    if entry.is_symlink() or not entry.is_file():
        raise ValueError("missing EFI selected entry")
    raw = entry.read_bytes()
    if len(raw) < 8 or len(raw) > 256:
        raise ValueError("invalid EFI entry bytes")
    try:
        name = raw[4:].decode("utf-16-le").rstrip("\x00")
    except UnicodeError as error:
        raise ValueError("invalid EFI UTF-16 selected entry") from error
    if not LOADER.fullmatch(name):
        raise ValueError("untrusted or unexpected EFI loader selection")
    return name, "root-" + LOADER.fullmatch(name).group(1)


def kernel_root(cmdline):
    if not isinstance(cmdline, str) or "\x00" in cmdline or len(cmdline) > 8192:
        raise ValueError("invalid kernel command line")
    roots = [token.removeprefix("root=") for token in cmdline.split()
             if token.startswith("root=")]
    if len(roots) != 1 or not roots[0].startswith("PARTUUID="):
        raise ValueError("unknown or ambiguous kernel root")
    identity = roots[0][9:].lower()
    if not UUID.fullmatch(identity):
        raise ValueError("invalid kernel root PARTUUID")
    return identity


def verify_boot_identity(identity, cmdline, efi_entry):
    if not isinstance(identity, dict) or set(identity) != {
        "schema", "slot", "root_partuuid", "source_commit",
        "input_manifest_sha256", "build_id"
    } or identity["schema"] != "flowerly-generation-root/v1":
        raise ValueError("invalid baked identity schema")
    if identity["slot"] not in ("root-a", "root-b"):
        raise ValueError("invalid baked root slot")
    if not isinstance(identity["root_partuuid"], str) or not UUID.fullmatch(identity["root_partuuid"]):
        raise ValueError("invalid baked root UUID")
    if not isinstance(identity["source_commit"], str) or not COMMIT.fullmatch(identity["source_commit"]):
        raise ValueError("invalid baked source commit")
    if not isinstance(identity["input_manifest_sha256"], str) or not SHA.fullmatch(identity["input_manifest_sha256"]):
        raise ValueError("invalid input manifest digest")
    if not isinstance(identity["build_id"], str) or not re.fullmatch(
        r"[A-Za-z0-9_.-]{1,64}", identity["build_id"]
    ):
        raise ValueError("invalid baked build ID")
    actual = kernel_root(cmdline)
    if actual != identity["root_partuuid"] or efi_entry != identity["slot"]:
        raise ValueError("EFI / kernel / baked root identity mismatch")
    return actual


def user_data_marker(path, create_first):
    """One random data-partition marker created only on first slot A boot."""
    target = Path(path)
    if target.is_symlink() or target.parent.is_symlink() or not target.parent.is_dir():
        raise ValueError("unsafe persistent data marker")
    if not target.exists():
        if not create_first:
            raise ValueError("persistent data marker missing after initial boot")
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                     os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
        try:
            data = os.urandom(32)
            if os.write(fd, data) != len(data):
                raise OSError("short persistent marker write")
            os.fsync(fd)
        finally:
            os.close(fd)
        directory = os.open(target.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    fd = os.open(target, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode) or os.fstat(fd).st_size != 32:
            raise ValueError("invalid persistent data marker")
        value = os.read(fd, 33)
    finally:
        os.close(fd)
    if len(value) != 32:
        raise ValueError("persistent data marker unreadable")
    return hashlib.sha256(value).hexdigest()


def append_witness(ledger, identity, cmdline, efi_bytes, boot_id,
                   root_device, data_device, marker_path=None):
    if root_device == data_device:
        raise ValueError("data ledger is not on a separate filesystem")
    if not isinstance(boot_id, str) or not re.fullmatch(
        r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", boot_id
    ):
        raise ValueError("invalid kernel boot ID")
    # EFI variable wire format is 4 bytes of attributes followed by UTF-16LE.
    if not isinstance(efi_bytes, bytes) or not 8 <= len(efi_bytes) <= 256:
        raise ValueError("invalid EFI entry variable")
    try:
        entry_name = efi_bytes[4:].decode("utf-16-le").rstrip("\x00")
    except UnicodeError as error:
        raise ValueError("invalid EFI selection encoding") from error
    match = LOADER.fullmatch(entry_name)
    if not match:
        raise ValueError("unexpected selected bootloader entry")
    slot = "root-" + match.group(1)
    partuuid = verify_boot_identity(identity, cmdline, slot)
    target = Path(ledger)
    if target.is_symlink() or target.parent.is_symlink() or not target.parent.is_dir():
        raise ValueError("unsafe boot-witness ledger path")
    fd = os.open(target, os.O_RDWR | os.O_CREAT | os.O_APPEND |
                 os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        if not stat.S_ISREG(os.fstat(fd).st_mode) or os.fstat(fd).st_size > MAX_LEDGER:
            raise ValueError("invalid boot-witness file")
        # Refuse incomplete or malformed previous records rather than hiding
        # a power-loss-corrupted ledger or inventing a new sequence.
        with os.fdopen(os.dup(fd), "rb") as current:
            previous = current.read(MAX_LEDGER + 1)
        if previous and not previous.endswith(b"\n"):
            raise ValueError("truncated previous witness record")
        lines = previous.splitlines()
        sequence = 1
        if lines:
            last = json.loads(lines[-1])
            if type(last.get("sequence")) is not int or last["sequence"] < 1:
                raise ValueError("invalid previous witness sequence")
            if any(json.loads(row).get("boot_id") == boot_id for row in lines):
                raise ValueError("duplicate kernel boot ID")
            sequence = last["sequence"] + 1
        marker_hash = (user_data_marker(marker_path, not lines and slot == "root-a")
                       if marker_path is not None else None)
        evidence = {
            "schema": "flowerly-generation-boot/v1",
            "boot_id": boot_id, "sequence": sequence, "slot": slot,
            "root_partuuid": partuuid, "loader_entry": entry_name,
            "source_commit": identity["source_commit"],
            "input_manifest_sha256": identity["input_manifest_sha256"],
            "build_id": identity["build_id"],
            "health": "NOT_YET_TESTED",
            "user_data_sha256": marker_hash,
        }
        output = (json.dumps(evidence, separators=(",", ":"), sort_keys=True) + "\n").encode()
        if len(output) > 2048 or os.write(fd, output) != len(output):
            raise OSError("incomplete witness append")
        os.fsync(fd)
        return evidence
    finally:
        os.close(fd)


def main():
    if sys.argv != [sys.argv[0], "witness"]:
        sys.exit("usage: generation-ab-guest.py witness")
    try:
        identity = strict_json(IDENTITY)
        cmdline = Path("/proc/cmdline").read_text()
        name = (Path("/sys/firmware/efi/efivars") /
                ("LoaderEntrySelected-" + EFI_LOADER_GUID))
        if name.is_symlink():
            raise ValueError("EFI variable is symlinked")
        efi_bytes = name.read_bytes()
        boot_id = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        data = LEDGER.parent
        # Explicitly refuse a rootfs-owned state directory, preventing a
        # successful witness when the persistent data partition failed to mount.
        evidence = append_witness(
            LEDGER, identity, cmdline, efi_bytes, boot_id,
            os.stat("/").st_dev, data.stat().st_dev,
            data / "generation-user-marker.bin")
    except (ValueError, OSError, TypeError, KeyError, json.JSONDecodeError) as error:
        sys.exit(f"Flowerly Generation witness refused: {error}")
    print("FLOWERLY_GENERATION_ROOT_VERIFIED")
    print("FLOWERLY_GENERATION_USER_MARKER_SHA256=" + evidence["user_data_sha256"])
    print(json.dumps({"boot_id": evidence["boot_id"],
                      "root_partuuid": evidence["root_partuuid"],
                      "slot": evidence["slot"]}, sort_keys=True))


if __name__ == "__main__":
    main()

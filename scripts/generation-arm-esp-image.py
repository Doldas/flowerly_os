#!/usr/bin/env python3
"""Prepare an A/B EFI B-boot candidate by publishing a NEW disposable raw disk.

No mounted ESP or block-device mutation. The source disk and user-data slice
are never written. This cannot stand in for a crash-safe production EFI arm.
"""

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import tempfile

MIB = 1048576
# Test-fixture power-loss points inside the arm sequence (payload copy, then the
# boot entry, then the loader default). None is the complete, real sequence.
ARM_FAULTS = (None, "before-entry", "tmp-entry", "entry-no-default", "torn-entry")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")


def digest(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as source:
        while chunk := source.read(MIB):
            hasher.update(chunk)
    return hasher.hexdigest()


def slice_digest(fd, offset, size):
    hasher = hashlib.sha256()
    remaining = size
    while remaining:
        block = os.pread(fd, min(MIB, remaining), offset)
        if not block:
            raise ValueError("short disk partition read")
        hasher.update(block)
        offset += len(block)
        remaining -= len(block)
    return hasher.hexdigest()


def safe_file(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file():
        raise ValueError("missing or symlinked input")
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        if not stat.S_ISREG(os.fstat(fd).st_mode):
            raise ValueError("refusing block device or special file")
    finally:
        os.close(fd)
    return path


def validate_partition_layout(disk, manifest):
    if (
        not isinstance(manifest, dict)
        or manifest.get("schema") != "flowerly-ab-disk/v1"
    ):
        raise ValueError("invalid A/B disk manifest")
    parts = manifest.get("partitions")
    if not isinstance(parts, list) or [p.get("name") for p in parts] != [
        "esp",
        "root-a",
        "root-b",
        "data",
    ]:
        raise ValueError("unexpected A/B partition order")
    declared = manifest.get("disk", {})
    if (
        type(declared.get("size_bytes")) is not int
        or declared["size_bytes"] != disk.stat().st_size
    ):
        raise ValueError("disk size mismatch")
    fd = os.open(disk, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        current = 0
        for p in parts:
            off, size = p.get("offset_bytes"), p.get("size_bytes")
            if (
                type(off) is not int
                or type(size) is not int
                or off < current
                or size < MIB
                or off % MIB
                or size % MIB
            ):
                raise ValueError("bad partition geometry")
            if (
                off + size > declared["size_bytes"]
                or not isinstance(p.get("partuuid"), str)
                or not UUID.fullmatch(p["partuuid"])
            ):
                raise ValueError("invalid GPT bounds/identity")
            if p["name"] in ("root-a", "root-b"):
                if slice_digest(fd, off, size) != p.get("sha256"):
                    raise ValueError("system root has changed unexpectedly")
            current = off + size
    finally:
        os.close(fd)
    # Validate the *actual GPT* in addition to the potentially stale runtime
    # disk manifest. ESP boot counters and persistent data are expected to
    # change on real boot; they are deliberately not hash-pinned here.
    sfdisk = shutil.which(
        "sfdisk", path=os.environ.get("PATH", "") + ":/usr/sbin:/sbin"
    )
    if not sfdisk:
        raise ValueError("missing actual sfdisk tool for GPT validation")
    result = subprocess.run(
        [sfdisk, "-J", str(disk)], text=True, capture_output=True, check=True
    )
    partition_table = json.loads(result.stdout)["partitiontable"]
    if partition_table.get("label") != "gpt":
        raise ValueError("not a GPT disk")
    actual = partition_table.get("partitions", [])
    if len(actual) != 4:
        raise ValueError("unexpected actual GPT partition count")
    for expected, part in zip(parts, actual):
        if (
            part.get("name") != expected["name"]
            or (part.get("uuid", "").lower() != expected["partuuid"])
            or part.get("start") != expected["offset_bytes"] // 512
            or (part.get("size") != expected["size_bytes"] // 512)
        ):
            raise ValueError("actual GPT differs from recorded partition identity")
    return {p["name"]: p for p in parts}


def run_mtools(*args):
    try:
        subprocess.run(
            list(args), check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE
        )
    except subprocess.CalledProcessError as error:
        raise ValueError("FAT edit rejected by mtools") from error


def arm_image(
    disk,
    manifest,
    kernel,
    initramfs,
    output,
    output_manifest,
    tries=3,
    failure_switch=False,
    inject_before_publish=False,
    fault=None,
):
    disk, manifest, kernel, initramfs = map(
        safe_file, (disk, manifest, kernel, initramfs)
    )
    output, output_manifest = Path(output), Path(output_manifest)
    input_paths = {p.resolve() for p in (disk, manifest, kernel, initramfs)}
    if output.resolve() == output_manifest.resolve() or any(
        p.resolve() in input_paths for p in (output, output_manifest)
    ):
        raise ValueError("output aliases an input or the manifest")
    if any(p.exists() or p.is_symlink() for p in (output, output_manifest)):
        raise ValueError("refusing to overwrite existing A/B output")
    if output.parent.is_symlink() or output_manifest.parent.is_symlink():
        raise ValueError("refusing symlinked output directory")
    if type(tries) is not int or not 1 <= tries <= 9:
        raise ValueError("invalid systemd-boot retry count")
    if fault not in ARM_FAULTS:
        raise ValueError("unknown arm fault point")
    if kernel.stat().st_size == 0 or initramfs.stat().st_size == 0:
        raise ValueError("missing kernel/initramfs")
    if manifest.stat().st_size > MIB:
        raise ValueError("oversized manifest")
    source_manifest = json.loads(manifest.read_text())
    parts = validate_partition_layout(disk, source_manifest)
    # Both root slots must be populated with a valid ext4 filesystem. The
    # original builder leaves B empty until a separate stage operation.
    with disk.open("rb") as image:
        for slot in ("root-a", "root-b"):
            image.seek(parts[slot]["offset_bytes"] + 1080)
            if image.read(2) != b"\x53\xef":
                raise ValueError(f"{slot} is not a staged ext4 root")
    esp = parts["esp"]
    root_b = parts["root-b"]
    with disk.open("rb") as original:
        data_before = slice_digest(
            original.fileno(),
            parts["data"]["offset_bytes"],
            parts["data"]["size_bytes"],
        )
    with tempfile.TemporaryDirectory(prefix="flowerly-arm-") as workspace:
        temp = Path(workspace)
        staging_disk = temp / "armed.img"
        shutil.copyfile(disk, staging_disk)
        esp_file = temp / "esp.img"
        with staging_disk.open("rb") as image, esp_file.open("wb") as dest:
            image.seek(esp["offset_bytes"])
            remaining = esp["size_bytes"]
            while remaining:
                data = image.read(min(MIB, remaining))
                if not data:
                    raise ValueError("short ESP extraction")
                dest.write(data)
                remaining -= len(data)
        # Existing A entry must remain; never rebuild the ESP from scratch.
        existing = subprocess.run(
            ["mdir", "-b", "-i", str(esp_file), "::/loader/entries"],
            text=True,
            capture_output=True,
            check=True,
        ).stdout
        if "flowerly-a" not in existing.lower():
            raise ValueError("no fallback slot A boot entry")
        if "flowerly-b" in existing.lower():
            raise ValueError(
                "B entry already exists; re-arming requires separate review"
            )
        run_mtools("mmd", "-i", str(esp_file), "::/flowerly/b")
        run_mtools("mcopy", "-i", str(esp_file), str(kernel), "::/flowerly/b/vmlinuz")
        run_mtools(
            "mcopy", "-i", str(esp_file), str(initramfs), "::/flowerly/b/initramfs.img"
        )
        entry = f"flowerly-b+{tries}.conf"
        entry_file = temp / entry
        flags = " flowerly.test.fail_health=1" if failure_switch else ""
        entry_text = (
            "title Flowerly OS slot B (test image)\n"
            "linux /flowerly/b/vmlinuz\n"
            "initrd /flowerly/b/initramfs.img\n"
            f"options root=PARTUUID={root_b['partuuid']} ro rootfstype=ext4 "
            "systemd.volatile=state fsck.mode=skip console=ttyS0 systemd.unit=multi-user.target loglevel=6"
            + flags
            + "\n"
        )
        if fault == "torn-entry":
            # A write cut off in the middle of the root PARTUUID.
            marker = "options root=PARTUUID="
            entry_text = entry_text[: entry_text.index(marker) + len(marker) + 13]
        entry_file.write_text(entry_text)
        # "tmp-entry" is the state just before the atomic rename: the entry
        # exists only under a name the loader ignores.
        esp_entry = entry + ".tmp" if fault == "tmp-entry" else entry
        entry_on_esp = fault != "before-entry"
        if entry_on_esp:
            run_mtools(
                "mcopy",
                "-i",
                str(esp_file),
                str(entry_file),
                "::/loader/entries/" + esp_entry,
            )
        # Default selector prefers B while its boot count is nonzero. When B
        # exhausts its count, systemd-boot must choose the known good A entry.
        # The pattern must match both slots: 'flowerly-b*' keeps selecting an
        # exhausted B entry (found by the sixth real KVM run).
        loader = temp / "loader.conf"
        loader.write_text("default flowerly-*\ntimeout 0\nconsole-mode keep\n")
        loader_updated = fault in (None, "torn-entry")
        if loader_updated:
            run_mtools(
                "mcopy",
                "-o",
                "-i",
                str(esp_file),
                str(loader),
                "::/loader/loader.conf",
            )
        fd = os.open(staging_disk, os.O_RDWR | os.O_NOFOLLOW)
        try:
            with esp_file.open("rb") as source:
                offset = esp["offset_bytes"]
                while block := source.read(MIB):
                    if os.pwrite(fd, block, offset) != len(block):
                        raise OSError("short ESP write")
                    offset += len(block)
            os.fsync(fd)
        finally:
            os.close(fd)
        new_manifest = copy.deepcopy(source_manifest)
        new_manifest["partitions"][0]["sha256"] = digest(esp_file)
        new_manifest["disk"]["sha256"] = digest(staging_disk)
        files = new_manifest.setdefault("esp_files", {})
        files["/flowerly/b/vmlinuz"] = digest(kernel)
        files["/flowerly/b/initramfs.img"] = digest(initramfs)
        if entry_on_esp:
            files["/loader/entries/" + esp_entry] = digest(entry_file)
        if loader_updated:
            files["/loader/loader.conf"] = digest(loader)
        if inject_before_publish:
            raise RuntimeError("injected power loss before publishing armed disk")
        with staging_disk.open("rb") as armed:
            data_after = slice_digest(
                armed.fileno(),
                parts["data"]["offset_bytes"],
                parts["data"]["size_bytes"],
            )
        if data_before != data_after:
            raise ValueError("data partition changed while arming EFI")
        # Publish only new paths, never overwrite the source image/manifest.
        # If a crash leaves a disk without its manifest, it is untrusted.
        with output_manifest.open("xb") as target:
            raw = (json.dumps(new_manifest, indent=2, sort_keys=True) + "\n").encode()
            target.write(raw)
            target.flush()
            os.fsync(target.fileno())
        try:
            with staging_disk.open("rb") as source, output.open("xb") as target:
                shutil.copyfileobj(source, target, MIB)
                target.flush()
                os.fsync(target.fileno())
        except Exception:
            output_manifest.unlink(missing_ok=True)
            raise
    return {
        "status": "ARMED_OFFLINE_NOT_BOOT_VERIFIED"
        if fault is None
        else "ARM_INTERRUPTED_FIXTURE_" + fault.upper().replace("-", "_"),
        "boot_entry": entry,
        "disk_sha256": new_manifest["disk"]["sha256"],
        "data_partition_sha256": data_before,
    }


def rollback_image(disk, manifest, output, output_manifest):
    """Test fixture for the future slot tool's rollback: make A the default
    again and delete B's boot entry on a NEW disposable disk.

    Roots and user data are never written; the source image and manifest are
    untouched. Slot B's kernel and initramfs stay on the ESP.
    """
    disk, manifest = map(safe_file, (disk, manifest))
    output, output_manifest = Path(output), Path(output_manifest)
    if any(p.exists() or p.is_symlink() for p in (output, output_manifest)):
        raise ValueError("refusing to overwrite existing rollback output")
    if output.resolve() == output_manifest.resolve():
        raise ValueError("output aliases the manifest")
    source_manifest = json.loads(manifest.read_text())
    parts = validate_partition_layout(disk, source_manifest)
    esp = parts["esp"]
    with tempfile.TemporaryDirectory(prefix="flowerly-rollback-") as workspace:
        temp = Path(workspace)
        staging_disk = temp / "rolled-back.img"
        shutil.copyfile(disk, staging_disk)
        esp_file = temp / "esp.img"
        with staging_disk.open("rb") as image, esp_file.open("wb") as dest:
            image.seek(esp["offset_bytes"])
            remaining = esp["size_bytes"]
            while remaining:
                data = image.read(min(MIB, remaining))
                if not data:
                    raise ValueError("short ESP extraction")
                dest.write(data)
                remaining -= len(data)
        listing = subprocess.run(
            ["mdir", "-b", "-i", str(esp_file), "::/loader/entries"],
            text=True,
            capture_output=True,
            check=True,
        ).stdout.split()
        names = [Path(x).name for x in listing]
        if not any(n.lower().startswith("flowerly-a") for n in names):
            raise ValueError("no slot A entry to roll back to")
        b_entries = [n for n in names if n.lower().startswith("flowerly-b")]
        if not b_entries:
            raise ValueError("no slot B entry to roll back")
        for name in b_entries:
            run_mtools("mdel", "-i", str(esp_file), "::/loader/entries/" + name)
        loader = temp / "loader.conf"
        loader.write_text("default flowerly-a*\ntimeout 0\nconsole-mode keep\n")
        run_mtools(
            "mcopy", "-o", "-i", str(esp_file), str(loader), "::/loader/loader.conf"
        )
        fd = os.open(staging_disk, os.O_RDWR | os.O_NOFOLLOW)
        try:
            with esp_file.open("rb") as source:
                offset = esp["offset_bytes"]
                while block := source.read(MIB):
                    if os.pwrite(fd, block, offset) != len(block):
                        raise OSError("short ESP write")
                    offset += len(block)
            os.fsync(fd)
        finally:
            os.close(fd)
        new_manifest = copy.deepcopy(source_manifest)
        new_manifest["partitions"][0]["sha256"] = digest(esp_file)
        new_manifest["disk"]["sha256"] = digest(staging_disk)
        files = new_manifest.setdefault("esp_files", {})
        for key in [k for k in files if k.startswith("/loader/entries/flowerly-b")]:
            del files[key]
        files["/loader/loader.conf"] = digest(loader)
        with output_manifest.open("xb") as target:
            target.write(
                (json.dumps(new_manifest, indent=2, sort_keys=True) + "\n").encode()
            )
            target.flush()
            os.fsync(target.fileno())
        try:
            with staging_disk.open("rb") as source, output.open("xb") as target:
                shutil.copyfileobj(source, target, MIB)
                target.flush()
                os.fsync(target.fileno())
        except Exception:
            output_manifest.unlink(missing_ok=True)
            raise
    return {
        "status": "ROLLED_BACK_TO_A_OFFLINE_FIXTURE",
        "removed_entries": sorted(b_entries),
        "disk_sha256": new_manifest["disk"]["sha256"],
    }


def main():
    p = argparse.ArgumentParser(description=__doc__)
    for key in ("disk", "manifest", "kernel", "initramfs", "output", "output-manifest"):
        p.add_argument("--" + key, type=Path, required=True)
    p.add_argument("--tries", type=int, default=3)
    p.add_argument(
        "--fail-health",
        action="store_true",
        help="KVM test fixture ONLY; B kernel line intentionally fails IPC health",
    )
    opts = p.parse_args()
    try:
        report = arm_image(
            opts.disk,
            opts.manifest,
            opts.kernel,
            opts.initramfs,
            opts.output,
            opts.output_manifest,
            tries=opts.tries,
            failure_switch=opts.fail_health,
        )
    except (
        OSError,
        RuntimeError,
        ValueError,
        TypeError,
        KeyError,
        subprocess.CalledProcessError,
        json.JSONDecodeError,
    ) as error:
        p.exit(1, f"A/B arm refused: {error}\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()

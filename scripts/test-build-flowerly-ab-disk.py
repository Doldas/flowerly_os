#!/usr/bin/env python3
"""Offline acceptance for the two-slot disk builder; no root, no KVM."""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/build-flowerly-ab-disk.sh"
MIB = 1048576

FAKES = {
    "mkfs.vfat": 'for last; do :; done; printf FATFS | dd of="$last" conv=notrunc status=none\n',
    "mkfs.ext4": 'for last; do :; done; printf EXT4DATA | dd of="$last" conv=notrunc status=none\n',
    "dumpe2fs": 'echo "Filesystem volume name:   ${FAKE_LABEL:-FLOWERLY_ROOT}"\n',
    "mmd": 'echo "mmd $*" >> "$FAKE_LOG"\n',
    "mcopy": 'echo "mcopy $*" >> "$FAKE_LOG"\n',
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def fixture(folder, volume_mb=2):
    fakes = folder / "fakes"
    fakes.mkdir()
    for name, body in FAKES.items():
        tool = fakes / name
        tool.write_text("#!/bin/sh\n" + body)
        tool.chmod(0o755)
    inputs = folder / "in"
    inputs.mkdir()
    volume = inputs / "root.ext4"
    volume.write_bytes(bytes(range(256)) * (volume_mb * MIB // 256))
    (inputs / "vmlinuz").write_bytes(b"kernel")
    (inputs / "initramfs.img").write_bytes(b"initrd")
    (inputs / "boot.efi").write_bytes(b"MZ-systemd-boot")
    return fakes, inputs


def run(folder, fakes, inputs, extra=None, env=None, output="disk.img"):
    environ = dict(os.environ, PATH=f"{fakes}:{os.environ['PATH']}",
                   FAKE_LOG=str(folder / "log"), FLOWERLY_ESP_MB="64",
                   FLOWERLY_DATA_MB="64", FLOWERLY_BUILD_COMMIT="c0ffee")
    environ.update(env or {})
    args = ["bash", str(SCRIPT), "--volume", str(inputs / "root.ext4"),
            "--kernel", str(inputs / "vmlinuz"), "--initramfs", str(inputs / "initramfs.img"),
            "--boot-efi", str(inputs / "boot.efi"), "--output", str(folder / output)]
    extra = [e.replace("{folder}", str(folder)) for e in extra or []]
    return subprocess.run(args + extra, env=environ, capture_output=True, text=True)


def success_case():
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        fakes, inputs = fixture(folder)
        result = run(folder, fakes, inputs)
        assert result.returncode == 0, result.stderr
        disk = (folder / "disk.img").read_bytes()
        manifest = json.loads((folder / "disk.img.manifest.json").read_text())
        assert manifest["schema"] == "flowerly-ab-disk/v1"
        assert manifest["builder_commit"] == "c0ffee"
        assert manifest["disk"]["sha256"] == sha(disk)
        assert manifest["disk"]["size_bytes"] == len(disk) == (1 + 64 + 2 + 2 + 64 + 1) * MIB
        parts = {p["name"]: p for p in manifest["partitions"]}
        assert list(parts) == ["esp", "root-a", "root-b", "data"]
        volume = (inputs / "root.ext4").read_bytes()
        for part in parts.values():
            blob = disk[part["offset_bytes"]:part["offset_bytes"] + part["size_bytes"]]
            assert sha(blob) == part["sha256"], part["name"]
        slot_a = parts["root-a"]
        assert disk[slot_a["offset_bytes"]:slot_a["offset_bytes"] + len(volume)] == volume
        slot_b = parts["root-b"]
        assert not any(disk[slot_b["offset_bytes"]:slot_b["offset_bytes"] + slot_b["size_bytes"]])
        assert len({p["partuuid"] for p in parts.values()}) == 4
        files = manifest["esp_files"]
        assert files["/flowerly/a/vmlinuz"] == sha(b"kernel")
        assert files["/EFI/BOOT/BOOTX64.EFI"] == sha(b"MZ-systemd-boot")
        assert "/loader/entries/flowerly-a+3.conf" in files
        log = (folder / "log").read_text()
        assert "::/loader/entries/flowerly-a+3.conf" in log
        assert not list(folder.glob(".flowerly-ab.*")), "work directory left behind"
        sfdisk = shutil.which("sfdisk", path=os.environ["PATH"] + ":/usr/sbin:/sbin")
        if sfdisk:
            dump = subprocess.run([sfdisk, "-d", str(folder / "disk.img")],
                                  capture_output=True, text=True, check=True).stdout.lower()
            assert "label: gpt" in dump
            for name in ("esp", "root-a", "root-b", "data"):
                assert f'name="{name}"' in dump, dump
            for part in parts.values():
                assert part["partuuid"] in dump
            assert "c12a7328-f81f-11d2-ba4b-00a0c93ec93b" in dump
            assert "4f68bce3-e8cd-4db1-96e7-fbcaf984b709" in dump
        else:
            print("GPT dump check skipped: sfdisk missing")
    print("Two-slot disk case passed: layout, readback hashes, empty slot B, manifest")


def entry_case():
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        fakes, inputs = fixture(folder)
        # make the fake mcopy keep the file so the loader entry can be read
        (fakes / "mcopy").write_text(
            '#!/bin/sh\necho "mcopy $*" >> "$FAKE_LOG"\n'
            'case "$4" in */flowerly-a+*.conf) cp "$3" "$(dirname "$FAKE_LOG")/entry.conf";; esac\n')
        result = run(folder, fakes, inputs, env={"FLOWERLY_BOOT_TRIES": "5"})
        assert result.returncode == 0, result.stderr
        manifest = json.loads((folder / "disk.img.manifest.json").read_text())
        assert "/loader/entries/flowerly-a+5.conf" in manifest["esp_files"]
        entry = (folder / "entry.conf").read_text()
        root_a = [p for p in manifest["partitions"] if p["name"] == "root-a"][0]["partuuid"]
        assert f"root=PARTUUID={root_a} " in entry and "console=ttyS0" in entry, entry
        assert "linux /flowerly/a/vmlinuz" in entry and "initrd /flowerly/a/initramfs.img" in entry
    print("Loader entry case passed: boot-count suffix and root PARTUUID")


def pinned_partuuid_case():
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        fakes, inputs = fixture(folder)
        root_a = "11111111-1111-1111-1111-111111111111"
        root_b = "22222222-2222-2222-2222-222222222222"
        (fakes / "mcopy").write_text(
            '#!/bin/sh\necho "mcopy $*" >> "$FAKE_LOG"\n'
            'case "$4" in */flowerly-a+*.conf) cp "$3" "$(dirname "$FAKE_LOG")/entry.conf";; esac\n')
        result = run(folder, fakes, inputs, env={
            "FLOWERLY_AB_ROOT_A_PARTUUID": root_a,
            "FLOWERLY_AB_ROOT_B_PARTUUID": root_b,
        })
        assert result.returncode == 0, result.stderr
        manifest = json.loads((folder / "disk.img.manifest.json").read_text())
        parts = {p["name"]: p for p in manifest["partitions"]}
        assert parts["root-a"]["partuuid"] == root_a
        assert parts["root-b"]["partuuid"] == root_b
        entry = (folder / "entry.conf").read_text()
        assert f"root=PARTUUID={root_a} " in entry
    print("Root A/B baked PARTUUIDs match GPT manifest and loader entry")


def refusal_cases():
    def expect(label, message, mutate=None, env=None, extra=None):
        with tempfile.TemporaryDirectory() as d:
            folder = Path(d)
            fakes, inputs = fixture(folder)
            if mutate:
                mutate(folder, fakes, inputs)
            result = run(folder, fakes, inputs, extra=extra, env=env)
            assert result.returncode != 0, label
            assert message in result.stderr, (label, result.stderr)
            assert not (folder / "disk.img").exists(), label
            assert not list(folder.glob(".flowerly-ab.*")), label
    expect("empty kernel", "missing, empty or symlinked",
           lambda f, k, i: (i / "vmlinuz").write_bytes(b""))
    expect("symlinked initramfs", "missing, empty or symlinked",
           lambda f, k, i: ((i / "initramfs.img").unlink(),
                            (i / "initramfs.img").symlink_to(i / "vmlinuz")))
    expect("non-PE loader", "not a PE/EFI binary",
           lambda f, k, i: (i / "boot.efi").write_bytes(b"ELF-nope"))
    expect("fractional MiB volume", "whole number of MiB",
           lambda f, k, i: (i / "root.ext4").write_bytes(b"x" * (MIB + 1)))
    expect("wrong label", "labelled FLOWERLY_ROOT", env={"FAKE_LABEL": "OTHER"})
    expect("esp too small", "FLOWERLY_ESP_MB", env={"FLOWERLY_ESP_MB": "8"})
    expect("bad tries", "FLOWERLY_BOOT_TRIES", env={"FLOWERLY_BOOT_TRIES": "0"})
    expect("invalid pinned UUID", "root slot PARTUUID",
           env={"FLOWERLY_AB_ROOT_A_PARTUUID": "not-a-uuid"})
    expect("duplicate pinned UUID", "must be distinct",
           env={"FLOWERLY_AB_ROOT_A_PARTUUID": "11111111-1111-1111-1111-111111111111",
                "FLOWERLY_AB_ROOT_B_PARTUUID": "11111111-1111-1111-1111-111111111111"})
    if not shutil.which("mcopy"):
        expect("missing tool", "missing host tool: mcopy",
               lambda f, k, i: (k / "mcopy").unlink())
    else:
        print("Missing-tool case skipped: mtools installed on this host")
    expect("same disk and manifest path", "must differ",
           lambda f, k, i: None, extra=["--manifest", "{folder}/disk.img"])
    print("Refusal cases passed: inputs, label, sizing, tools")


def overwrite_case():
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        fakes, inputs = fixture(folder)
        (folder / "disk.img").write_bytes(b"precious")
        result = run(folder, fakes, inputs)
        assert result.returncode != 0 and "refusing to overwrite" in result.stderr
        assert (folder / "disk.img").read_bytes() == b"precious"
        (folder / "disk.img").unlink()
        (folder / "disk.img.manifest.json").write_text("precious")
        result = run(folder, fakes, inputs)
        assert result.returncode != 0 and "refusing to overwrite" in result.stderr
        assert (folder / "disk.img.manifest.json").read_text() == "precious"
        assert not (folder / "disk.img").exists()
    print("Overwrite case passed: existing disk and manifest are never replaced")


if __name__ == "__main__":
    success_case()
    entry_case()
    pinned_partuuid_case()
    refusal_cases()
    overwrite_case()

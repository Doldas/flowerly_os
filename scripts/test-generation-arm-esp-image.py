#!/usr/bin/env python3
"""Real FAT/GPT offline acceptance for safe A/B boot image arming (#48)."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "arm", ROOT / "generation-arm-esp-image.py"
)
arm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(arm)
MIB = 1048576
A_UUID = "11111111-1111-1111-1111-111111111111"
B_UUID = "22222222-2222-2222-2222-222222222222"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def tool(name):
    path = shutil.which(name, path=os.environ.get("PATH", "") + ":/usr/sbin:/sbin")
    if not path:
        raise RuntimeError(f"required real host test tool missing: {name}")
    return path


def run(*args, input=None):
    return subprocess.run(args, input=input, text=True, check=True, capture_output=True)


def write_at(disk, start, path):
    with disk.open("r+b") as out, path.open("rb") as inp:
        out.seek(start * MIB)
        shutil.copyfileobj(inp, out)


def fixture(folder):
    # Real GPT+FAT and two ext4 roots. No mount privileges or KVM needed.
    esp = folder / "esp.img"
    with esp.open("wb") as out:
        out.truncate(64 * MIB)
    run(tool("mkfs.vfat"), "-F", "32", str(esp))
    run(
        tool("mmd"),
        "-i",
        str(esp),
        "::/loader",
        "::/loader/entries",
        "::/flowerly",
        "::/flowerly/a",
    )
    loader = folder / "loader.conf"
    loader.write_text("default flowerly-a*\ntimeout 0\n")
    entry = folder / "flowerly-a+3.conf"
    entry.write_text("title slot A\noptions root=PARTUUID=" + A_UUID + "\n")
    run(tool("mcopy"), "-i", str(esp), str(loader), "::/loader/loader.conf")
    run(
        tool("mcopy"), "-i", str(esp), str(entry), "::/loader/entries/flowerly-a+3.conf"
    )
    roots = []
    for index in range(2):
        root = folder / f"root-{index}.ext4"
        with root.open("wb") as out:
            out.truncate(2 * MIB)
        run(tool("mkfs.ext4"), "-q", "-F", "-L", "FLOWERLY_ROOT", str(root))
        roots.append(root)
    data = folder / "data.img"
    with data.open("wb") as out:
        out.truncate(2 * MIB)
    run(tool("mkfs.ext4"), "-q", "-F", "-L", "FLOWERLY_DATA", str(data))
    disk = folder / "disk.img"
    with disk.open("wb") as out:
        out.truncate(72 * MIB)
    layout = """label: gpt
unit: sectors
first-lba: 2048

start=2048, size=131072, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee, name="esp"
start=133120, size=4096, type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, uuid=11111111-1111-1111-1111-111111111111, name="root-a"
start=137216, size=4096, type=4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709, uuid=22222222-2222-2222-2222-222222222222, name="root-b"
start=141312, size=4096, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=dddddddd-dddd-dddd-dddd-dddddddddddd, name="data"
"""
    run(tool("sfdisk"), "--quiet", str(disk), input=layout)
    for start, src in ((1, esp), (65, roots[0]), (67, roots[1]), (69, data)):
        write_at(disk, start, src)
    parts = [
        ("esp", 1, 64, esp, "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"),
        ("root-a", 65, 2, roots[0], A_UUID),
        ("root-b", 67, 2, roots[1], B_UUID),
        ("data", 69, 2, data, "dddddddd-dddd-dddd-dddd-dddddddddddd"),
    ]
    manifest = {
        "schema": "flowerly-ab-disk/v1",
        "disk": {"size_bytes": disk.stat().st_size, "sha256": arm.digest(disk)},
        "partitions": [
            {
                "name": name,
                "offset_bytes": start * MIB,
                "size_bytes": size * MIB,
                "partuuid": uuid,
                "sha256": arm.digest(src),
            }
            for name, start, size, src, uuid in parts
        ],
        "esp_files": {"/loader/loader.conf": arm.digest(loader)},
    }
    manifest_path = folder / "manifest.json"
    manifest_path.write_text(json.dumps(manifest))
    kernel = folder / "vmlinuz-b"
    initramfs = folder / "initrd-b"
    kernel.write_bytes(b"kernel-b")
    initramfs.write_bytes(b"initramfs-b")
    return disk, manifest_path, kernel, initramfs


class ArmTests(unittest.TestCase):
    def test_real_fat_arming_preserves_source_and_userdata(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            disk, manifest, kernel, initramfs = fixture(folder)
            before = disk.read_bytes()
            output, out_manifest = folder / "armed.img", folder / "armed.json"
            report = arm.arm_image(
                disk,
                manifest,
                kernel,
                initramfs,
                output,
                out_manifest,
                tries=3,
                failure_switch=True,
            )
            self.assertEqual(report["status"], "ARMED_OFFLINE_NOT_BOOT_VERIFIED")
            armed = output.read_bytes()
            self.assertEqual(armed[:MIB], before[:MIB])
            self.assertEqual(
                armed[65 * MIB :],
                before[65 * MIB :],
                "root partitions or persistent data were changed",
            )
            self.assertNotEqual(armed[MIB : 65 * MIB], before[MIB : 65 * MIB])
            self.assertEqual(disk.read_bytes(), before, "input image changed")
            updated = json.loads(out_manifest.read_text())
            self.assertEqual(updated["disk"]["sha256"], sha(armed))
            esp = folder / "armed-esp.img"
            esp.write_bytes(armed[MIB : 65 * MIB])
            entry = run(
                tool("mtype"), "-i", str(esp), "::/loader/entries/flowerly-b+3.conf"
            ).stdout
            self.assertIn("root=PARTUUID=" + B_UUID, entry)
            self.assertIn("flowerly.test.fail_health=1", entry)
            loader = run(tool("mtype"), "-i", str(esp), "::/loader/loader.conf").stdout
            self.assertIn("default flowerly-b*", loader)
            self.assertEqual(updated["partitions"][0]["sha256"], sha(esp.read_bytes()))

    def test_fault_before_publish_never_modifies_disk(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            disk, manifest, kernel, initramfs = fixture(folder)
            before = disk.read_bytes()
            with self.assertRaisesRegex(RuntimeError, "power loss"):
                arm.arm_image(
                    disk,
                    manifest,
                    kernel,
                    initramfs,
                    folder / "missing.img",
                    folder / "missing.json",
                    inject_before_publish=True,
                )
            self.assertEqual(disk.read_bytes(), before)
            self.assertFalse((folder / "missing.img").exists())
            self.assertFalse((folder / "missing.json").exists())

    def test_active_root_tampering_and_outputs_are_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            disk, manifest, kernel, initramfs = fixture(folder)
            with disk.open("r+b") as out:
                out.seek(65 * MIB + 4096)
                out.write(b"wrong")
            with self.assertRaisesRegex(ValueError, "system root"):
                arm.arm_image(
                    disk,
                    manifest,
                    kernel,
                    initramfs,
                    folder / "armed.img",
                    folder / "armed.json",
                )
            self.assertFalse((folder / "armed.img").exists())


if __name__ == "__main__":
    unittest.main()

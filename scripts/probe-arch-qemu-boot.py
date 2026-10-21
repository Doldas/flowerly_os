#!/usr/bin/env python3
"""Direct-kernel QEMU boot probe of a populated Arch ext4 root volume.

This is NOT a partitioned/standalone bootable disk image. QEMU boots the
kernel and initramfs extracted from the associated rootfs. The disk is
opened read-only from the host's perspective (QEMU snapshot mode).
"""
import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DIR = ROOT / "target/flowerly-arch-rootfs"


def fail(message):
    raise ValueError(message)


def run(args):
    rootfs = args.rootfs.resolve()
    volume = args.volume.resolve()
    if not args.rootfs.is_dir() or args.rootfs.is_symlink():
        fail("missing or symlinked Arch rootfs")
    if not args.volume.is_file() or args.volume.is_symlink():
        fail("missing or symlinked ext4 root volume")
    os_release = rootfs / "usr/lib/os-release"
    if not os_release.is_file() or not re.search(
        r"(?m)^ID=arch$", os_release.read_text(encoding="utf-8")
    ):
        fail("rootfs does not identify Arch Linux")
    for name in ("usr/lib/systemd/systemd", "usr/bin/flowerlyd"):
        if not (rootfs / name).is_file():
            fail(f"missing OS component: {name}")
    # mkosi/Arch can retain the packaged kernel beneath usr/lib/modules
    # rather than copying it to /boot in a directory image. Never fall back
    # to the host kernel: it must come from this exact built rootfs.
    kernels = [rootfs / "boot/vmlinuz-linux"]
    kernels.extend(sorted((rootfs / "usr/lib/modules").glob("*/vmlinuz")))
    existing = [p for p in kernels if p.is_file() and not p.is_symlink() and p.stat().st_size]
    if len(existing) != 1:
        fail(
            "expected exactly one kernel in this rootfs (/boot/vmlinuz-linux "
            "or /usr/lib/modules/*/vmlinuz); found "
            f"{len(existing)}. Inspect boot/ and usr/lib/modules/."
        )
    kernel = existing[0]
    # mkosi's temporary module initrd is not necessarily a full userspace
    # initramfs or retained in the directory rootfs. Do not guess or use
    # one from the host; require an actual initramfs from this build.
    initramfs = rootfs / "boot/initramfs-linux.img"
    if not initramfs.is_file() or initramfs.is_symlink() or initramfs.stat().st_size == 0:
        fail(
            "missing rootfs boot/initramfs-linux.img; mkosi's temporary "
            "kernel-modules initrd is not a verified full initramfs. "
            "Build and stage an Arch initramfs for this exact rootfs first."
        )
    if not (10 <= args.timeout <= 600):
        fail("timeout must be 10..600 seconds")
    qemu = shutil.which("qemu-system-x86_64")
    if not qemu:
        fail("qemu-system-x86_64 is required")
    if args.log.exists() or args.log.is_symlink():
        fail(f"refusing to overwrite serial log: {args.log}")
    args.log.parent.mkdir(parents=True, exist_ok=True)
    command = [
        qemu, "-machine", "q35,accel=tcg", "-m", "2048",
        "-smp", "2", "-nographic", "-no-reboot", "-monitor", "none",
        "-kernel", str(kernel), "-initrd", str(initramfs),
        "-append",
        "root=/dev/vda rw rootfstype=ext4 console=ttyS0 "
        "systemd.unit=multi-user.target loglevel=6",
        "-drive", f"file={volume},format=raw,if=virtio,snapshot=on",
        "-net", "none",
    ]
    with args.log.open("xb") as output:
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                stdout=output, stderr=subprocess.STDOUT)
        try:
            proc.wait(timeout=args.timeout)
            fail(f"QEMU exited before boot observation (exit {proc.returncode})")
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
    log = args.log.read_text(encoding="utf-8", errors="replace")
    if any(marker in log for marker in ("Kernel panic", "Failed to start Flowerly OS Unified Supervisor")):
        fail("kernel panic or Flowerly supervisor failure detected; inspect serial log")
    if not re.search(r"Reached target .*Multi-User System", log):
        fail(f"systemd multi-user boot not observed within {args.timeout}s; inspect {args.log}")
    print(f"Direct QEMU kernel boot reached systemd multi-user target; serial log: {args.log}")
    print("NOT proof of complete Flowerly service health or bootable standalone disk.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rootfs", type=Path, default=DEFAULT_DIR / "flowerly-rootfs")
    parser.add_argument("--volume", type=Path, default=DEFAULT_DIR / "flowerly-root.ext4")
    parser.add_argument("--log", type=Path, default=DEFAULT_DIR / "qemu-serial.log")
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    try:
        run(args)
    except (ValueError, OSError) as exc:
        print(f"QEMU boot probe: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

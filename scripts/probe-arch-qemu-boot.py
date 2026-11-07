#!/usr/bin/env python3
"""Direct-kernel QEMU boot probe of a populated Arch ext4 root volume.

This is NOT a partitioned/standalone bootable disk image. QEMU boots the
kernel and initramfs extracted from the associated rootfs. The disk is
opened read-only from the host's perspective (QEMU snapshot mode).
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DIR = ROOT / "target/flowerly-arch-rootfs"
KVM_DEVICE = Path(os.environ.get("FLOWERLY_KVM_DEVICE", "/dev/kvm"))


def fail(message):
    raise ValueError(message)


def artifact_identity(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return {"path": str(path), "bytes": path.stat().st_size,
            "sha256": digest.hexdigest()}


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
    if args.report is not None:
        if args.report.exists() or args.report.is_symlink():
            fail(f"refusing to overwrite evidence report: {args.report}")
        if args.report.resolve() == args.log.resolve():
            fail("evidence report and serial log must differ")
    if args.accel == "kvm" and not os.access(KVM_DEVICE, os.R_OK | os.W_OK):
        fail(f"--accel kvm needs read-write access to {KVM_DEVICE}; use tcg or fix group membership")
    args.log.parent.mkdir(parents=True, exist_ok=True)
    command = [
        qemu, "-machine", f"q35,accel={args.accel}", "-m", "2048",
        *(["-cpu", "host"] if args.accel == "kvm" else []), "-smp", "2", "-nographic", "-no-reboot", "-monitor", "none",
        "-kernel", str(kernel), "-initrd", str(initramfs),
        "-append",
        "root=/dev/vda rw rootfstype=ext4 console=ttyS0 "
        "systemd.unit=multi-user.target loglevel=6",
        "-drive", f"file={volume},format=raw,if=virtio,snapshot=on",
        "-net", "none",
    ]
    started = time.monotonic()
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
    # Require the witness emitted by boot-ipc-health.sh on its own line.
    # A quoted command, diagnostic or partial token is not boot evidence.
    # systemd prefixes console output of journal+console units with the kernel
    # timestamp and the writing process: "[    7.52] bash[348]: <line>".
    if not re.search(
        r"(?m)^(?:\[\s*\d+\.\d+\] bash\[\d+\]: )?FLOWERLY_BOOT_IPC_HEALTHY\r?$", log
    ):
        fail("Flowerly IPC health witness not observed; inspect serial log")
    if args.report is not None:
        evidence = {
            "schema": 1,
            "probe": f"direct-kernel-qemu-{args.accel}",
            "accepted": True,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "timeout_seconds": args.timeout,
            "command": command,
            "artifacts": {
                "volume": artifact_identity(volume),
                "kernel": artifact_identity(kernel),
                "initramfs": artifact_identity(initramfs),
                "serial_log": artifact_identity(args.log),
            },
            "limitations": "Boot and one IPC Ping only; not sustained health or a standalone disk",
        }
        args.report.parent.mkdir(parents=True, exist_ok=True)
        with args.report.open("x", encoding="utf-8") as output:
            json.dump(evidence, output, sort_keys=True, indent=2)
            output.write("\n")
    print(f"Direct QEMU boot reached multi-user target and Flowerly IPC health witness; serial log: {args.log}")
    print("Witness means flowerlyd answered IPC Ping at boot, NOT sustained health or a standalone bootable disk.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rootfs", type=Path, default=DEFAULT_DIR / "flowerly-rootfs")
    parser.add_argument("--volume", type=Path, default=DEFAULT_DIR / "flowerly-root.ext4")
    parser.add_argument("--log", type=Path, default=DEFAULT_DIR / "qemu-serial.log")
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--accel", choices=("tcg", "kvm"), default="tcg",
                        help="QEMU accelerator; kvm needs a read-write /dev/kvm")
    parser.add_argument("--report", type=Path, help="write JSON evidence; never overwrite")
    args = parser.parse_args()
    try:
        run(args)
    except (ValueError, OSError) as exc:
        print(f"QEMU boot probe: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

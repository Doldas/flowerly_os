#!/usr/bin/env python3
"""Offline mkosi initrd staging contract: no packages or root access."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/build-arch-initrd.sh"
OUTPUT = ROOT / "target/flowerly-arch-initrd/flowerly-initrd.cpio.zst"


def main():
    if OUTPUT.parent.exists():
        raise SystemExit("refusing to touch an existing initrd build directory")
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        rootfs = base / "rootfs"
        (rootfs / "usr/lib").mkdir(parents=True)
        (rootfs / "usr/lib/os-release").write_text("ID=arch\n")
        bindir = base / "bin"
        bindir.mkdir()
        stub = bindir / "mkosi"
        stub.write_text(
            "#!/bin/sh\n"
            "printf '%s\\n' \"$@\" > \"$MKOSI_ARGS\"\n"
            "mkdir -p \"$(dirname \"$MKOSI_OUTPUT\")\"\n"
            "printf 'mock compressed cpio' > \"$MKOSI_OUTPUT\"\n"
        )
        stub.chmod(0o755)
        env = dict(os.environ)
        env.update(PATH=str(bindir) + os.pathsep + os.environ["PATH"],
                   MKOSI_ARGS=str(base / "args"),
                   MKOSI_OUTPUT=str(OUTPUT))
        try:
            command = ["bash", str(SCRIPT), str(rootfs)]
            first = subprocess.run(command, cwd=ROOT, env=env,
                                   capture_output=True, text=True)
            assert first.returncode == 0, first.stderr
            args = (base / "args").read_text().splitlines()
            assert args[-1] == "build" and any(
                "arch-initrd" in arg for arg in args if arg.startswith("--directory=")
            ), args
            dest = rootfs / "boot/initramfs-linux.img"
            assert dest.read_bytes() == b"mock compressed cpio"
            second = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
            assert second.returncode != 0, "existing initramfs overwritten"
            dest.unlink()
            (rootfs / "usr/lib/os-release").write_text("ID=debian\n")
            third = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
            assert third.returncode != 0, "non-Arch rootfs accepted"
        finally:
            if OUTPUT.exists():
                OUTPUT.unlink()
            if OUTPUT.parent.exists():
                OUTPUT.parent.rmdir()
    print("Initrd builder: staging, no overwrite and distro validation verified offline")


if __name__ == "__main__":
    main()

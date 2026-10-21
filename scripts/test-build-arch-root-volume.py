#!/usr/bin/env python3
"""Offline and rootless acceptance for the ext4 root-volume builder."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/build-arch-root-volume.sh"


def setup(rootfs):
    for path in ("usr/lib/os-release", "usr/lib/systemd/systemd",
                 "usr/bin/flowerlyd", "usr/lib/systemd/system/flowerlyd.service"):
        target = rootfs / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("ID=arch\n" if path.endswith("os-release") else "fixture\n")


def run(rootfs, image, env):
    return subprocess.run(["bash", str(SCRIPT), str(rootfs), str(image)],
                          env=env, capture_output=True, text=True)


def main():
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        rootfs = folder / "arch"
        image = folder / "volume.ext4"
        setup(rootfs)
        bin_dir = folder / "mockbin"
        bin_dir.mkdir()
        (bin_dir / "mkfs.ext4").write_text(
            "#!/bin/sh\n"
            "printf '%s\\n' \"$@\" > \"$ARGS_LOG\"\n"
            "exit 0\n"
        )
        (bin_dir / "dumpe2fs").write_text(
            "#!/bin/sh\n"
            "echo 'Filesystem volume name:   FLOWERLY_ROOT'\n"
        )
        for name in ("mkfs.ext4", "dumpe2fs"):
            (bin_dir / name).chmod(0o755)
        log = folder / "arguments"
        env = dict(os.environ, PATH=str(bin_dir) + ":" + os.environ["PATH"],
                   FLOWERLY_ROOT_SIZE_MB="512", ARGS_LOG=str(log))
        result = run(rootfs, image, env)
        assert result.returncode == 0, result.stderr
        assert image.is_file() and image.stat().st_size == 512 * 1024 * 1024
        args = log.read_text().splitlines()
        assert "-d" in args and str(rootfs) in args and "FLOWERLY_ROOT" in args, args
        original_size = image.stat().st_size
        assert run(rootfs, image, env).returncode != 0, "existing artifact overwritten"
        assert image.stat().st_size == original_size
        image.unlink()
        (rootfs / "usr/lib/os-release").write_text("ID=debian\n")
        assert run(rootfs, image, env).returncode != 0, "non-Arch rootfs accepted"
        (rootfs / "usr/lib/os-release").write_text("ID=arch\n")
        env["FLOWERLY_ROOT_SIZE_MB"] = "64"
        assert run(rootfs, image, env).returncode != 0, "undersized image accepted"
        print("Ext4 volume: population command, size and fail-closed checks pass")


if __name__ == "__main__":
    main()

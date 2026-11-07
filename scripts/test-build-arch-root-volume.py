#!/usr/bin/env python3
"""Offline and rootless acceptance for the ext4 root-volume builder."""
from pathlib import Path
import os
import shutil
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


def real_ownership_case():
    """Real mkfs.ext4: files owned by subordinate ids must keep their owner and mode."""
    path = os.environ["PATH"] + ":/usr/sbin:/sbin"
    if not all(shutil.which(t, path=path) for t in ("mkfs.ext4", "debugfs", "dumpe2fs")):
        print("Ext4 ownership case skipped: e2fsprogs missing")
        return
    root = os.geteuid() == 0
    enter = [] if root else ["podman", "unshare"]
    if enter and (not shutil.which("podman") or
                  subprocess.run(enter + ["true"], capture_output=True).returncode != 0):
        print("Ext4 ownership case skipped: no usable 'podman unshare'")
        return
    with tempfile.TemporaryDirectory() as d:
        folder = Path(d)
        rootfs = folder / "arch"
        image = folder / "volume.ext4"
        setup(rootfs)
        helper = rootfs / "usr/lib/dbus-1.0/dbus-daemon-launch-helper"
        secret = rootfs / "etc/private"
        for directory in (helper.parent, secret):
            directory.mkdir(parents=True, exist_ok=True)
        helper.write_text("helper\n")
        (secret / "key").write_text("k\n")
        # root:dbus setuid helper, and a 0700 directory owned by a service id
        shell = ("chown 0:81 {h} && chmod 4750 {h} && chown 5:5 {s}/key {s} "
                 "&& chmod 0600 {s}/key && chmod 0700 {s}").format(h=helper, s=secret)
        subprocess.run(enter + ["sh", "-c", shell], check=True)
        try:
            env = dict(os.environ, FLOWERLY_ROOT_SIZE_MB="512")
            result = run(rootfs, image, env)
            assert result.returncode == 0, result.stderr
            assert root or "podman unshare" in result.stderr, result.stderr
            def stat(inner):
                out = subprocess.run(["debugfs", "-R", "stat " + inner, str(image)],
                                     capture_output=True, text=True, env=dict(os.environ, PATH=path)).stdout
                return out
            out = stat("/usr/lib/dbus-1.0/dbus-daemon-launch-helper")
            assert "Mode:  04750" in out and "User:     0   Group:    81" in out, out
            out = stat("/etc/private/key")
            assert "Mode:  0600" in out and "User:     5   Group:     5" in out, out
            assert "Mode:  0700" in stat("/etc/private")
        finally:
            subprocess.run(enter + ["rm", "-rf", str(rootfs)], check=False)
    print("Ext4 ownership: unreadable subordinate-id files and setuid helper preserved")


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
    real_ownership_case()


if __name__ == "__main__":
    main()

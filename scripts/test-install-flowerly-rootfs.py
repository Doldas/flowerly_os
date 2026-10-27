#!/usr/bin/env python3
"""Offline regression for Flowerly binary/service staging into Arch rootfs."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
INSTALLER = ROOT / "scripts/install-flowerly-rootfs.sh"
BINARIES = ("agenticctl", "evolutiond", "flowerlyd")


def invoke(rootfs, binaries):
    return subprocess.run(["bash", str(INSTALLER), str(rootfs), str(binaries)],
                          capture_output=True, text=True)


def main():
    with tempfile.TemporaryDirectory() as d:
        tmp = Path(d)
        rootfs = tmp / "rootfs"
        bins = tmp / "release"
        (rootfs / "usr/lib").mkdir(parents=True)
        bins.mkdir()
        for name in BINARIES:
            binary = bins / name
            binary.write_bytes(b"fake offline regression binary\n")
            binary.chmod(0o755)
        assert invoke(rootfs, bins).returncode != 0, "non-Arch rootfs accepted"
        (rootfs / "usr/lib/os-release").write_text("ID=arch\n")
        (bins / "evolutiond").unlink()
        assert invoke(rootfs, bins).returncode != 0, "missing binary accepted"
        (bins / "evolutiond").write_bytes(b"fake offline regression binary\n")
        (bins / "evolutiond").chmod(0o755)
        result = invoke(rootfs, bins)
        assert result.returncode == 0, result.stderr
        for name in BINARIES:
            assert (rootfs / "usr/bin" / name).is_file()
            assert os.access(rootfs / "usr/bin" / name, os.X_OK)
        for name in ("flowerlyd", "evolutiond"):
            assert (rootfs / "usr/lib/systemd/system" / (name + ".service")).is_file()
            assert not (rootfs / "etc/systemd/system/multi-user.target.wants" / (name + ".service")).exists()
        # The multi-user drop-in is staged for administrators, never active.
        assert (rootfs / "usr/share/flowerly/systemd/flowerlyd-multi-user.conf").is_file()
        assert not (rootfs / "usr/lib/systemd/system/flowerlyd.service.d").exists()
        assert not (rootfs / "etc/systemd/system/flowerlyd.service.d").exists()
        users = (rootfs / "usr/lib/sysusers.d/flowerly.conf").read_text()
        assert "u flowerly " in users and "u flowerly-evolution " in users
        print("Flowerly rootfs installation: missing inputs rejected, units staged but not enabled, multi-user drop-in staged inactive")


if __name__ == "__main__":
    main()

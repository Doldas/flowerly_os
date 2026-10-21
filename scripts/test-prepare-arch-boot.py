#!/usr/bin/env python3
"""Test boot preparation offline without root privileges or VM boot."""
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
PREPARE = ROOT / "scripts/prepare-arch-boot.sh"


def invoke(rootfs):
    return subprocess.run(["bash", str(PREPARE), str(rootfs)],
                          capture_output=True, text=True)


def populate(rootfs):
    paths = [
        "usr/lib/os-release", "usr/lib/systemd/systemd",
        "usr/bin/systemd-sysusers", "usr/bin/flowerlyd",
        "usr/bin/agenticctl",
        "usr/lib/systemd/system/flowerlyd.service",
        "usr/lib/sysusers.d/flowerly.conf",
    ]
    for name in paths:
        dest = rootfs / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text("ID=arch\n" if name.endswith("os-release") else "fixture\n")
        if name in ("usr/lib/systemd/systemd", "usr/bin/flowerlyd", "usr/bin/systemd-sysusers"):
            dest.chmod(0o755)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        rootfs = Path(tmp) / "arch"
        populate(rootfs)
        link = rootfs / "etc/systemd/system/multi-user.target.wants/flowerlyd.service"
        assert invoke(rootfs).returncode == 0
        assert link.is_symlink() and link.readlink() == Path("/usr/lib/systemd/system/flowerlyd.service")
        assert not (link.parent / "evolutiond.service").exists()
        witness = link.parent / "flowerly-boot-witness.service"
        assert witness.is_symlink()
        assert (rootfs / "usr/lib/systemd/system/flowerly-boot-witness.service").is_file()
        assert invoke(rootfs).returncode != 0, "double enablement accepted"
    with tempfile.TemporaryDirectory() as tmp:
        rootfs = Path(tmp) / "arch"
        populate(rootfs)
        (rootfs / "usr/lib/sysusers.d/flowerly.conf").unlink()
        assert invoke(rootfs).returncode != 0, "missing users accepted"
    with tempfile.TemporaryDirectory() as tmp:
        rootfs = Path(tmp) / "arch"
        populate(rootfs)
        (rootfs / "usr/lib/os-release").write_text("ID=debian\n")
        assert invoke(rootfs).returncode != 0, "wrong distro accepted"
    print("Arch boot preparation: valid enablement and negative safety checks pass")


if __name__ == "__main__":
    main()

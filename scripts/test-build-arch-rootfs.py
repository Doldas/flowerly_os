#!/usr/bin/env python3
"""Offline contract checks for Arch directory-rootfs verification; no package downloads."""
from pathlib import Path
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/build-arch-rootfs.sh"
ARTIFACT = ROOT / "target/flowerly-arch-rootfs/flowerly-rootfs"


def test(arch, complete):
    with tempfile.TemporaryDirectory() as tmp:
        mock = Path(tmp)
        (mock / "pacman").write_text("#!/bin/sh\nexit 0\n")
        (mock / "mkosi").write_text(
            "#!/bin/sh\n"
            "mkdir -p \"$FAKE_ARTIFACT/usr/lib/systemd\" \"$FAKE_ARTIFACT/usr/bin\"\n"
            f"echo ID={'arch' if arch else 'other'} > \"$FAKE_ARTIFACT/usr/lib/os-release\"\n"
            + ("touch \"$FAKE_ARTIFACT/usr/lib/systemd/systemd\" \"$FAKE_ARTIFACT/usr/bin/pacman\"\n"
               if complete else "")
        )
        for exe in ("pacman", "mkosi"):
            (mock / exe).chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = str(mock) + os.pathsep + os.environ["PATH"]
        env["FAKE_ARTIFACT"] = str(ARTIFACT)
        return subprocess.run(["bash", str(SCRIPT)], cwd=ROOT, env=env,
                              capture_output=True, text=True)


def main():
    if ARTIFACT.parent.exists():
        raise SystemExit("test refuses to touch existing rootfs output directory")
    try:
        assert test(arch=True, complete=True).returncode == 0
        shutil.rmtree(ARTIFACT.parent)
        assert test(arch=False, complete=True).returncode != 0
        shutil.rmtree(ARTIFACT.parent)
        assert test(arch=True, complete=False).returncode != 0
        print("Arch rootfs: valid substrate accepted; wrong distro/incomplete artifact rejected")
    finally:
        if ARTIFACT.parent.exists():
            shutil.rmtree(ARTIFACT.parent)


if __name__ == "__main__":
    main()

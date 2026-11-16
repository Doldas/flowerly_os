#!/usr/bin/env python3
"""Offline contract checks for Arch directory-rootfs verification; no package downloads."""

from pathlib import Path
import json
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/build-arch-rootfs.sh"
ARTIFACT = ROOT / "target/flowerly-arch-rootfs/flowerly-rootfs"


def test(arch, complete, pinned=False, args_file=None):
    with tempfile.TemporaryDirectory() as tmp:
        mock = Path(tmp)
        (mock / "pacman").write_text("#!/bin/sh\nexit 0\n")
        (mock / "mkosi").write_text(
            "#!/bin/sh\n"
            'echo "$@" > "$MKOSI_ARGS"\n'
            'mkdir -p "$FAKE_ARTIFACT/usr/lib/systemd" "$FAKE_ARTIFACT/usr/bin"\n'
            f'echo ID={"arch" if arch else "other"} > "$FAKE_ARTIFACT/usr/lib/os-release"\n'
            + (
                'touch "$FAKE_ARTIFACT/usr/lib/systemd/systemd" "$FAKE_ARTIFACT/usr/bin/pacman"\n'
                if complete
                else ""
            )
        )
        for exe in ("pacman", "mkosi"):
            (mock / exe).chmod(0o755)
        env = dict(os.environ)
        env["PATH"] = str(mock) + os.pathsep + os.environ["PATH"]
        env["FAKE_ARTIFACT"] = str(ARTIFACT)
        env["MKOSI_ARGS"] = str(args_file or mock / "args")
        if pinned:
            env["FLOWERLY_PINNED"] = "1"
        return subprocess.run(
            ["bash", str(SCRIPT)], cwd=ROOT, env=env, capture_output=True, text=True
        )


def main():
    if ARTIFACT.parent.exists():
        raise SystemExit("test refuses to touch existing rootfs output directory")
    try:
        assert test(arch=True, complete=True).returncode == 0
        shutil.rmtree(ARTIFACT.parent)
        assert test(arch=False, complete=True).returncode != 0
        shutil.rmtree(ARTIFACT.parent)
        assert test(arch=True, complete=False).returncode != 0
        shutil.rmtree(ARTIFACT.parent)
        with tempfile.TemporaryDirectory() as tmp:
            args = Path(tmp) / "args"
            assert test(arch=True, complete=True, args_file=args).returncode == 0
            assert "--mirror" not in args.read_text()
            shutil.rmtree(ARTIFACT.parent)
            assert (
                test(arch=True, complete=True, pinned=True, args_file=args).returncode
                == 0
            )
            manifest = json.loads(
                (ROOT / "distro/base-image/arch/inputs.manifest.json").read_text()
            )
            snapshot = manifest["source"]["snapshot_url"].rstrip("/")
            assert f"--mirror={snapshot}" in args.read_text().split(), args.read_text()
        print(
            "Arch rootfs: valid substrate accepted; wrong distro/incomplete artifact rejected; pinned mode uses the manifest snapshot"
        )
    finally:
        if ARTIFACT.parent.exists():
            shutil.rmtree(ARTIFACT.parent)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Offline tests for IPC-gated Flowerly boot witness."""
from pathlib import Path
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "distro/overlay/usr/lib/flowerly/boot-ipc-health.sh"


def invoke(executable, attempts):
    env = dict(os.environ, FLOWERLY_BOOT_AGENTICCTL=str(executable),
               FLOWERLY_BOOT_PING_ATTEMPTS=str(attempts))
    return subprocess.run(["bash", str(SCRIPT)], env=env, capture_output=True, text=True)


def main():
    with tempfile.TemporaryDirectory() as directory:
        mock = Path(directory) / "agenticctl"
        mock.write_text("#!/bin/sh\nexit 0\n")
        mock.chmod(0o755)
        ok = invoke(mock, 1)
        assert ok.returncode == 0, ok.stderr
        assert ok.stdout.strip() == "FLOWERLY_BOOT_IPC_HEALTHY"
        mock.write_text("#!/bin/sh\nexit 1\n")
        failed = invoke(mock, 1)
        assert failed.returncode != 0 and "FLOWERLY_BOOT_IPC_HEALTHY" not in failed.stdout
        invalid = invoke(mock, 0)
        assert invalid.returncode != 0
        print("Flowerly boot IPC witness: positive, failed ping and invalid limits")


if __name__ == "__main__":
    main()

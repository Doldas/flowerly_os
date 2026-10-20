#!/usr/bin/env python3
"""CLI-to-real-flowerlyd contract for the local, advisory AI model catalog.

Exercises the CLI's actual JSON args, server validation, kernel-bound actor,
durable operation replay, missing evidence and retirement across restart.
No network access, provider process, GPU dependency or root privileges.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid


def main(binary_directory):
    cli = binary_directory / "agenticctl"
    daemon = binary_directory / "flowerlyd"
    with tempfile.TemporaryDirectory(prefix="flowerly-cli-compute-") as temp:
        root = Path(temp)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        log_file = root / "flowerlyd.log"
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime),
                   AGENTIC_STATE_DIR=str(state))
        processes = []

        def invoke(*arguments, ok=True):
            result = subprocess.run(
                [str(cli), "--runtime-dir", str(runtime), *arguments],
                env=env, capture_output=True, text=True, timeout=12,
                check=False,
            )
            if not ok:
                assert result.returncode != 0, (arguments, result.stdout)
                return result.stdout + result.stderr
            assert result.returncode == 0, (
                arguments, result.returncode, result.stderr, log_file.read_text()
            )
            return json.loads(result.stdout)

        def start():
            with log_file.open("a") as log:
                process = subprocess.Popen(
                    [str(daemon)], env=env, stdout=log, stderr=subprocess.STDOUT
                )
            processes.append(process)
            for _ in range(150):
                if process.poll() is not None:
                    raise AssertionError(f"daemon exited: {log_file.read_text()}")
                if (runtime / "flowerlyd.sock").exists():
                    try:
                        if invoke("ping")["pong"] is True:
                            return process
                    except (OSError, subprocess.TimeoutExpired, AssertionError):
                        pass
                time.sleep(0.05)
            raise AssertionError(f"daemon not ready: {log_file.read_text()}")

        def stop(process):
            process.terminate()
            try:
                process.wait(timeout=6)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=6)

        try:
            process = start()
            assert invoke("model", "list")["models"] == []
            budget = invoke("device", "budget")
            assert budget["enforcement"]["cpu_memory"] == "advisory-measured"
            assert budget["enforcement"]["gpu_vram"] == "not-enforced"

            key = "cli-compute-" + uuid.uuid4().hex
            registration = (
                "model", "register", "Local evidence example",
                "--backend", "CPU",
                "--context-tokens", "2048",
                "--modality", "text", "--modality", "vision",
                "--evidence", "operator-recorded",
                "--operation-id", key,
            )
            model = invoke(*registration)
            model_id = model["id"]
            assert model_id.startswith("model-"), model
            assert model["modalities"] == ["text", "vision"], model
            assert invoke(*registration) == model, "identical operation must replay"
            assert invoke("model", "show", model_id)["created_by"] == (
                f"uid:{os.getuid()}"
            ), "CLI must not be able to forge the daemon's authenticated actor"
            assert [item["id"] for item in invoke("model", "list")["models"]] == [
                model_id
            ]

            compatibility = invoke("model", "compatibility", model_id)
            assert compatibility["verdict"] == "UNKNOWN", compatibility
            assert compatibility["promise"] == "advisory-only", compatibility
            assert any(
                "memory" in reason for reason in compatibility["reasons"]
            ), compatibility

            conflict = invoke(
                "model", "register", "Changed payload",
                "--operation-id", key, ok=False,
            )
            assert "Conflict" in conflict, conflict
            assert "InvalidArgument" in invoke(
                "model", "register", "Invalid backend",
                "--backend", "NOT_A_BACKEND", ok=False,
            )
            assert "NotFound" in invoke(
                "model", "show", "model-does-not-exist", ok=False,
            )

            retirement = invoke("model", "retire", model_id)
            assert retirement == {"id": model_id, "retired": True}, retirement
            assert invoke("model", "list")["models"] == []
            assert invoke("model", "show", model_id)["retired_at"] is not None
            assert invoke("model", "compatibility", model_id)["verdict"] == "UNKNOWN"

            stop(process)
            process = start()
            assert invoke("model", "show", model_id)["retired_at"] is not None
            assert invoke(*registration) == model, "replay survives restart"
            assert invoke("model", "list")["models"] == [], "replay must not unretire"
            assert invoke("ping")["pong"] is True
            print("CLI model registration, replay, retirement, honest budget and restart: OK")
        finally:
            for process in processes:
                if process.poll() is None:
                    stop(process)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-cli-compute.py target/release")
    main(Path(sys.argv[1]).resolve())

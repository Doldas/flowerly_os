#!/usr/bin/env python3
"""Black-box RFC 0004 compute-foundation acceptance.

Drives the real flowerlyd supervisor over the authenticated local socket and
the shared durable SQLite state: model catalog registration (idempotent),
strict metadata validation, honest device budget reporting, compatibility
verdicts without invented numbers, soft retirement and restart durability.

The catalog grants no OS authority: no command here touches processes,
filesystem or network. Local-only; no provider or endpoint is configured.
"""
import json
import os
import pathlib
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise AssertionError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def request(socket_path, name, args):
    correlation = str(uuid.uuid4())
    message = json.dumps({
        "version": 1,
        "id": correlation,
        "type": "command",
        "name": name,
        "args": args,
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        assert size < 8 * 1024 * 1024, size
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, (name, reply)
    assert reply["version"] == 1, (name, reply)
    return reply


def shutdown(socket_path):
    correlation = str(uuid.uuid4())
    message = json.dumps({"version": 1, "id": correlation, "type": "shutdown"}).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation and reply["type"] == "ok", reply


def wait_ready(socket_path, process, log_path):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited: {log_path.read_text()}")
        if socket_path.exists():
            try:
                reply = request(socket_path, "flow.list", {})
                if reply["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def assert_error(sock, name, args, expected):
    reply = request(sock, name, args)
    assert reply["type"] == "err", (name, reply)
    if expected is not None:
        assert reply["code"] == expected, (name, reply)
    return reply["message"]


def main(bin_dir):
    sock = None
    with tempfile.TemporaryDirectory(prefix="flowerly-compute-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        # Admin commands and IPC shutdown need an explicit admin group (#31).
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state),
                   AGENTIC_ADMIN_GROUP=str(os.getgid()))
        log_path = root / "flowerlyd.log"
        processes = []

        def start():
            with log_path.open("ab") as log:
                proc = subprocess.Popen(
                    [str(bin_dir / "flowerlyd")],
                    env=env, stdout=log, stderr=subprocess.STDOUT,
                )
            processes.append(proc)
            wait_ready(runtime / "flowerlyd.sock", proc, log_path)
            return runtime / "flowerlyd.sock"

        try:
            sock = start()

            # 1. Honest device budget: real /proc/meminfo parsing or an explicit
            #    absence label; unenforceable limits are named, never hidden.
            budget = request(sock, "device.budget", {})["data"]
            expected_source = os.path.exists("/proc/meminfo")
            assert budget["meminfo_source_available"] == expected_source, budget
            if budget["meminfo_source_available"]:
                assert budget["total_memory_bytes"] > 0, budget
                assert budget["available_memory_bytes"] > 0, budget
            else:
                assert any(
                    not budget["available_memory_bytes"] for _ in [0]
                ), "absent source must not fabricate numbers"
            assert budget["enforcement"]["gpu_vram"] == "not-enforced", budget
            assert budget["enforcement"]["cpu_memory"] == "advisory-measured", budget

            # 2. Strict validation fail-closed with typed error codes.
            for args, code in [
                ({"name": " ", "backend": "CPU"}, "invalid_argument"),
                ({"name": "m", "backend": "MAGIC"}, "invalid_argument"),
                ({"name": "m", "measured_memory_bytes": 1.5}, "invalid_argument"),
                ({"name": "m", "measured_memory_bytes": -4}, "invalid_argument"),
                ({"name": "m", "modalities": [5]}, "invalid_argument"),
                ({"name": "m", "operation_id": "bad key"}, "invalid_argument"),
            ]:
                assert_error(sock, "model.register", args, code)
            empty_list = request(sock, "model.list", {})["data"]["models"]
            assert empty_list == [], "refused registrations must not persist"

            # 3. Registration with measured evidence; replay-safe operation id.
            operation_id = "compute-model-" + uuid.uuid4().hex
            registered = request(sock, "model.register", {
                "name": "qwen2-coder-14b",
                "family": "qwen2",
                "architecture": "transformer",
                "backend": "CPU",
                "context_tokens": 32768,
                "measured_memory_bytes": 9_000_000_000,
                "modalities": ["text"],
                "license": "apache-2.0",
                "evidence": "measure:gold-rss",
                "operation_id": operation_id,
                "created_by": "uid:1000",
            })["data"]
            model_id = registered["id"]
            assert model_id.startswith("model-"), registered
            replayed = request(sock, "model.register", {
                "name": "qwen2-coder-14b",
                "family": "qwen2",
                "architecture": "transformer",
                "backend": "CPU",
                "context_tokens": 32768,
                "measured_memory_bytes": 9_000_000_000,
                "modalities": ["text"],
                "license": "apache-2.0",
                "evidence": "measure:gold-rss",
                "operation_id": operation_id,
                "created_by": "uid:1000",
            })["data"]
            assert replayed["id"] == model_id, (model_id, replayed)
            listed = request(sock, "model.list", {})["data"]["models"]
            assert len(listed) == 1 and listed[0]["id"] == model_id, listed

            # 4. Compatibility verdicts are honest: measured fits or exceeds,
            #    and missing evidence is UNKNOWN with reasons, never an estimate.
            compat = request(sock, "model.compatibility", {"id": model_id})["data"]
            assert compat["verdict"] in ("FITS", "EXCEEDS_BUDGET"), compat
            assert compat["promise"] == "advisory-only", compat
            evidence_light = request(sock, "model.register", {
                "name": "unmeasured-model",
                "created_by": "uid:1000",
            })["data"]
            compat = request(
                sock, "model.compatibility", {"id": evidence_light["id"]}
            )["data"]
            assert compat["verdict"] == "UNKNOWN", compat
            assert any("memory" in reason for reason in compat["reasons"]), compat

            # 5. Unknown ids are typed NotFound, including retirement.
            for name in ("model.show", "model.compatibility", "model.retire"):
                message = assert_error(
                    sock, name,
                    {"id": "model-" + uuid.uuid4().hex},
                    "not_found",
                )
                assert "not found" in message, message

            # 6. Soft retirement keeps provenance and full retrievability.
            request(sock, "model.retire", {"id": model_id})
            listed = request(sock, "model.list", {})["data"]["models"]
            assert all(item["id"] != model_id for item in listed), listed
            detail = request(sock, "model.show", {"id": model_id})["data"]
            assert detail["retired_at"], detail

            # 7. Restart durability over the shared SQLite state.
            shutdown(sock)
            sock = start()
            detail = request(sock, "model.show", {"id": model_id})["data"]
            assert detail["retired_at"], detail
            replayed_again = request(sock, "model.register", {
                "name": "qwen2-coder-14b",
                "family": "qwen2",
                "architecture": "transformer",
                "backend": "CPU",
                "context_tokens": 32768,
                "measured_memory_bytes": 9_000_000_000,
                "modalities": ["text"],
                "license": "apache-2.0",
                "evidence": "measure:gold-rss",
                "operation_id": operation_id,
                "created_by": "uid:1000",
            })["data"]
            assert replayed_again["id"] == model_id, replayed_again

            budget = request(sock, "device.budget", {})["data"]
            assert budget["enforcement"]["gpu_vram"] == "not-enforced", budget
        finally:
            for proc in processes:
                if proc.poll() is None:
                    proc.terminate()
            for proc in processes:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
            sock = None
    print("Compute catalog black box: registration, replay, honest verdicts, "
          "retirement and restart durability verified")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        paths = "(usage: test-compute-catalog.py <binary_dir>)"
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

#!/usr/bin/env python3
"""Gate B acceptance: structured machine-readable error codes for compute.

Exercises every RFC 0004 compute command family's error paths through the
real supervisor and asserts each refusal carries a typed snake_case error
code (invalid_argument / not_found / conflict / unavailable /
permission_denied). No command may fail silently, panic, or return an
untouched exception string without a code.
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
        "version": 1, "id": correlation, "type": "command",
        "name": name, "args": args,
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, (name, reply)
    return reply


def wait_ready(socket_path, process, log_path):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited: {log_path.read_text()}")
        if socket_path.exists():
            try:
                if request(socket_path, "flow.list", {})["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def assert_error(reply, code, name):
    assert reply["type"] == "err", (name, reply)
    assert reply["code"] == code, (name, reply.get("code"), reply)
    assert isinstance(reply.get("message"), str) and reply["message"], (name, reply)


def main(bin_dir):
    with tempfile.TemporaryDirectory(prefix="flowerly-error-codes-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        log = root / "flowerlyd.log"
        proc = subprocess.Popen(
            [str(bin_dir / "flowerlyd")], env=env,
            stdout=log.open("ab"), stderr=subprocess.STDOUT,
        )
        sock = runtime / "flowerlyd.sock"
        try:
            wait_ready(sock, proc, log)

            unknown_id = "model-" + uuid.uuid4().hex
            checks = [
                # Model catalog classifications.
                ("model.register", {"name": " "}, "invalid_argument", "model.register blank"),
                ("model.register", {"name": "m", "backend": "WAT"}, "invalid_argument", "model.register backend"),
                ("model.register", {"name": "m", "measured_memory_bytes": 1.5}, "invalid_argument", "model.register float"),
                ("model.show", {"id": unknown_id}, "not_found", "model.show unknown"),
                ("model.show", {"id": "flow-123"}, "invalid_argument", "model.show bad id"),
                # Profile classifications.
                ("profile.create", {"name": "p", "toml": "api_key = 'x'\n"}, "invalid_argument", "profile credentials"),
                ("profile.create", {"name": "p", "toml": "unknown_key = 1\n"}, "invalid_argument", "profile unknown key"),
                # Provider classifications.
                ("provider.register", {"name": "remote", "kind": "openai-api", "capabilities": ["text"]},
                 "permission_denied", "provider remote kind"),
                # Workload classifications.
                ("workload.submit", {"profile_id": "profile-none", "model_id": "model-" + uuid.uuid4().hex},
                 "not_found", "workload missing profile"),
                # Usage classifications.
                ("usage.record", {"workload_id": "work-" + uuid.uuid4().hex, "tokens_in": 1, "tokens_out": 1,
                  "latency_ms": 1, "cost_micros": 1}, "not_found", "usage missing workload"),
                # Inference classifications.
                ("inference.request", {"workload_id": "work-" + uuid.uuid4().hex, "provider_id": "provider-" + uuid.uuid4().hex,
                  "prompt": "hi", "request_id": "r-1"}, "not_found", "inference missing workload"),
            ]
            for family, args, code, name in checks:
                reply = request(sock, family, args)
                assert_error(reply, code, name)

            # Duplicate profile name is a typed conflict.
            request(sock, "profile.create", {"name": "dup", "toml": "fallback='ask'\n"})
            reply = request(sock, "profile.create", {"name": "dup", "toml": "fallback='fail'\n"})
            assert_error(reply, "conflict", "profile duplicate name")

            # Operation replay with changed payload is a typed conflict.
            op = "errcode-" + uuid.uuid4().hex
            request(sock, "model.register", {
                "name": "replay-model", "created_by": "uid:1000", "operation_id": op,
            })
            reply = request(sock, "model.register", {
                "name": "replay-model-changed", "created_by": "uid:1000", "operation_id": op,
            })
            assert_error(reply, "conflict", "replay payload mismatch")

            # Unknown commands stay typed invalid_argument.
            for name in ["model.delete", "provider.connect", "workload.execute",
                         "usage.spam", "inference.chat"]:
                assert_error(request(sock, name, {}), "invalid_argument", name)

            # Healthy commands remain ok (codes are for failures, not noise).
            ok = request(sock, "device.budget", {})
            assert ok["type"] == "ok", ok
            ok = request(sock, "model.list", {})
            assert ok["type"] == "ok", ok
        finally:
            if proc.poll() is None:
                proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
    print("Compute error-code surface: every refusal carries a typed "
          "machine-readable code")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

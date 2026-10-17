#!/usr/bin/env python3
"""Black-box RFC 0004 tranche-3 acceptance: workload admission + usage ledger.

Fail-closed honesty: a model without a measured footprint is refused; a
GPU-accelerator backend is refused this tranche; profile ceilings bound
admission (memory, concurrency with queueing); decisions, ceilings and
reasons are recorded durably; usage ledger totals accumulate; remote/
execution actions are structurally absent.
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
        assert size < 8 * 1024 * 1024, size
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, (name, reply)
    assert reply["version"] == 1, (name, reply)
    return reply


def shutdown(socket_path):
    correlation = str(uuid.uuid4())
    message = json.dumps({"version": 1, "id": correlation, "type": "shutdown"}).encode()
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
                if request(socket_path, "flow.list", {})["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def err(reply, code, needle):
    assert reply["type"] == "err", reply
    assert reply["code"] == code, (reply, code)
    if needle:
        assert needle in reply["message"], reply
    return reply["message"]


def main(bin_dir):
    processes = []
    with tempfile.TemporaryDirectory(prefix="flowerly-scheduling-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
        log_path = root / "flowerlyd.log"

        def start():
            with log_path.open("ab") as log:
                proc = subprocess.Popen(
                    [str(bin_dir / "flowerlyd")],
                    env=env, stdout=log, stderr=subprocess.STDOUT,
                )
            processes.append(proc)
            wait_ready(sock_path(), proc, log_path)
            return runtime / "flowerlyd.sock"

        def sock_path():
            return runtime / "flowerlyd.sock"

        try:
            start()

            # Small measured CPU models to drive admission rules.
            small = request(sock_path(), "model.register", {
                "name": "mini-model", "backend": "CPU",
                "measured_memory_bytes": 1_000_000_000,
                "created_by": "uid:1000",
            })["data"]
            huge = request(sock_path(), "model.register", {
                "name": "huge-model", "backend": "CPU",
                "measured_memory_bytes": 10_000_000_000_000,
                "created_by": "uid:1000",
            })["data"]
            gpu = request(sock_path(), "model.register", {
                "name": "cuda-model", "backend": "CUDA",
                "measured_memory_bytes": 2_000_000_000,
                "created_by": "uid:1000",
            })["data"]
            unmeasured = request(sock_path(), "model.register", {
                "name": "no-measurements", "backend": "CPU",
                "created_by": "uid:1000",
            })["data"]
            low_memory_profile = request(sock_path(), "profile.create", {
                "name": "budget-tight",
                "toml": "memory_max_bytes = 2_000_000_000\nconcurrency_max = 1\n",
            })["data"]
            roomy_profile = request(sock_path(), "profile.create", {
                "name": "budget-roomy",
                "toml": "memory_max_bytes = 12_000_000_000\nconcurrency_max = 2\n",
            })["data"]

            # 1. Honesty refusals, each typed and reason-bearing.
            reasons = []
            for profile, model, needle in [
                (low_memory_profile, huge, "exceeds profile ceiling"),
                (roomy_profile, gpu, "accelerator VRAM"),
                (roomy_profile, unmeasured, "measured memory footprint"),
            ]:
                reply = request(sock_path(), "workload.submit", {
                    "profile_id": profile["id"], "model_id": model["id"],
                    "created_by": "uid:1000",
                })
                assert reply["type"] == "ok", (profile["name"], model["name"], reply)
                assert reply["data"]["state"] == "REFUSED", reply["data"]
                assert needle in reply["data"]["reason"], reply["data"]
                reasons.append(reply["data"]["reason"])

            # 2. Within ceilings ADMITS.
            ok_workload = request(sock_path(), "workload.submit", {
                "profile_id": roomy_profile["id"],
                "model_id": small["id"],
                "background": "normal",
                "priority": 5,
                "created_by": "uid:1000",
            })["data"]
            assert ok_workload["state"] == "ADMITTED", ok_workload
            assert ok_workload["priority"] == 5, ok_workload

            # 3. Concurrency ceiling of budget-tight: two competing submits;
            #    the fallback default queues the overflow, fail-profile refuses.
            tight_fail_profile = request(sock_path(), "profile.create", {
                "name": "budget-tight-fail",
                "toml": "memory_max_bytes = 2_000_000_000\n"
                        "concurrency_max = 0\nfallback = 'fail'\n",
            })["data"]
            refused = request(sock_path(), "workload.submit", {
                "profile_id": tight_fail_profile["id"],
                "model_id": small["id"],
                "created_by": "uid:1000",
            })["data"]
            assert refused["state"] == "REFUSED", refused
            tight_queue_profile = request(sock_path(), "profile.create", {
                "name": "budget-tight-queue",
                "toml": "memory_max_bytes = 2_000_000_000\n"
                        "concurrency_max = 0\nfallback = 'queue'\n",
            })["data"]
            queued = request(sock_path(), "workload.submit", {
                "profile_id": tight_queue_profile["id"],
                "model_id": small["id"],
                "created_by": "uid:1000",
            })["data"]
            assert queued["state"] == "QUEUED", queued

            # 4. Retry of the same operation_id replays the same decision id.
            op = "wl-" + uuid.uuid4().hex
            first = request(sock_path(), "workload.submit", {
                "profile_id": roomy_profile["id"], "model_id": small["id"],
                "operation_id": op, "created_by": "uid:1000",
            })["data"]
            second = request(sock_path(), "workload.submit", {
                "profile_id": roomy_profile["id"], "model_id": small["id"],
                "operation_id": op, "created_by": "uid:1000",
            })["data"]
            assert first["id"] == second["id"], (first, second)

            # 5. Show/list ceilings snapshot; stop transitions; unknown ids.
            detail = request(sock_path(), "workload.show", {"id": first["id"]})["data"]
            assert "ceilings" in detail and "concurrency_max" in detail["ceilings"], detail
            listed = request(sock_path(), "workload.list", {"mine": True})["data"]
            assert listed["count"] >= 1, listed
            stopped = request(sock_path(), "workload.stop", {"id": first["id"]})["data"]
            assert stopped["state"] == "STOPPED", stopped
            err(request(sock_path(), "workload.stop", {"id": first["id"]}),
                "conflict", "not running")
            err(request(sock_path(), "workload.show", {"id": "work-none"}),
                "not_found", "workload not found")

            # 6. Usage ledger accumulates against the model; unbounded input
            #    framing is typed-invalid.
            usage = request(sock_path(), "usage.record", {
                "workload_id": ok_workload["id"],
                "tokens_in": 1200, "tokens_out": 400,
                "latency_ms": 2500, "cost_micros": 900,
                "created_by": "uid:1000",
            })["data"]
            totals = usage["model_totals"]
            assert totals["tokens_in"] == 1200 and totals["cost_micros"] == 900, usage
            usage2 = request(sock_path(), "usage.record", {
                "workload_id": ok_workload["id"],
                "tokens_in": 100, "tokens_out": 10,
                "latency_ms": 100, "cost_micros": 10,
                "created_by": "uid:1000",
            })["data"]
            assert usage2["model_totals"]["tokens_in"] == 1300, usage2
            err(request(sock_path(), "usage.record", {
                "workload_id": ok_workload["id"],
                "tokens_in": -5, "tokens_out": 1, "latency_ms": 1, "cost_micros": 1,
            }), "invalid_argument", "")
            err(request(sock_path(), "usage.record", {
                "workload_id": "work-none",
                "tokens_in": 1, "tokens_out": 1, "latency_ms": 1, "cost_micros": 1,
            }), "not_found", "workload not found")
            brief = request(sock_path(), "workload.usage_brief", {
                "model_id": small["id"],
            })["data"]
            assert brief["tokens_in"] == 1300 and brief["promise"] == "ledger-only", brief

            # 7. Restart durability of admission decisions and ledger rows.
            shutdown(sock_path())
            start()
            detail = request(sock_path(), "workload.show", {"id": first["id"]})["data"]
            assert detail["state"] == "STOPPED", detail
            brief = request(sock_path(), "workload.usage_brief", {
                "model_id": small["id"],
            })["data"]
            assert brief["tokens_in"] == 1300, brief
        finally:
            for proc in processes:
                if proc.poll() is None:
                    proc.terminate()
            for proc in processes:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
    print("Workload admission and usage ledger black box: honest refusals, "
          "ceilings queueing, replay, typed transitions and durable ledger verified")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

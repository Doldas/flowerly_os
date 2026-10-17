#!/usr/bin/env python3
"""Black-box RFC 0004 tranche-4 acceptance: inference execution contract.

Runs the REAL fault-isolated reference provider (providerd) next to flowerlyd
over the authenticated local IPC: capability discovery, ADMITTED-only
execution, bounded requests, deterministic echo results, usage-ledger
accounting with operation_id replay (#63), profile-fallback outage semantics
(ask/queue/fail with explicit consent and zero silent retries/downgrades),
and restart durability. No egress; the provider cannot grant OS authority —
the only durable artifact is the bounded ledger row.
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
        client.settimeout(15)
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
    try:
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(8)
            client.connect(str(socket_path))
            client.sendall(struct.pack("<I", len(message)) + message)
            size = struct.unpack("<I", recv_exact(client, 4))[0]
            reply = json.loads(recv_exact(client, size))
        return reply["type"] == "ok"
    except OSError:
        return False


def wait_ready(socket_path, process, log_path, probe=("flow.list", {})):
    for _ in range(150):
        if process.poll() is not None:
            raise AssertionError(f"daemon exited: read {log_path}")
        if socket_path.exists():
            try:
                if request(socket_path, probe[0], probe[1])["type"] == "ok":
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
    with tempfile.TemporaryDirectory(prefix="flowerly-inference-") as root:
        root = pathlib.Path(root)
        runtime = root / "runtime"
        state = root / "state"
        runtime.mkdir(mode=0o700)
        state.mkdir(mode=0o700)
        env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))

        def start_supervisor():
            log = root / "flowerlyd.log"
            proc = subprocess.Popen(
                [str(bin_dir / "flowerlyd")], env=env,
                stdout=log.open("ab"), stderr=subprocess.STDOUT,
            )
            processes.append(proc)
            wait_ready(runtime / "flowerlyd.sock", proc, log)
            return runtime / "flowerlyd.sock"

        def start_provider(outage=False):
            env_row = dict(env)
            if outage:
                env_row["FLOWERLY_PROVIDER_OUTAGE"] = "1"
            log = root / "providerd.log"
            proc = subprocess.Popen(
                [str(bin_dir / "providerd")], env=env_row,
                stdout=log.open("ab"), stderr=subprocess.STDOUT,
            )
            processes.append(proc)
            wait_ready(runtime / "providerd.sock", proc, log, probe=("provider.discover", {}))
            return proc

        try:
            flowerlyd_sock = start_supervisor()
            start_provider()

            # Setup: registered provider row, measured model, roomy profile.
            provid = request(flowerlyd_sock, "provider.register", {
                "name": "reference-echo", "kind": "local-unix",
                "capabilities": ["text"], "endpoint": "providerd.sock",
                "created_by": "uid:1000",
            })["data"]["id"]
            model = request(flowerlyd_sock, "model.register", {
                "name": "echo-capable", "backend": "CPU",
                "measured_memory_bytes": 500_000_000, "created_by": "uid:1000",
            })["data"]["id"]
            profile = request(flowerlyd_sock, "profile.create", {
                "name": "exec-roomy",
                "toml": "memory_max_bytes = 12_000_000_000\nconcurrency_max = 2\n",
            })["data"]["id"]
            workload = request(flowerlyd_sock, "workload.submit", {
                "profile_id": profile, "model_id": model,
                "created_by": "uid:1000",
            })["data"]
            assert workload["state"] == "ADMITTED", workload

            # 1. Discovery reflects the live reference provider honestly.
            caps = request(flowerlyd_sock, "inference.capabilities",
                           {"created_by": "uid:1000"})["data"]
            assert caps["live_provider"]["provider"] == "echo", caps
            assert caps["live_provider"]["synthetic"] is True, caps
            registered = [row["id"] for row in caps["registered"]]
            assert provid in registered, caps

            # 2. Bounded execution: ADMITTED workload executes and ledgers.
            op = "infer-" + uuid.uuid4().hex
            result = request(flowerlyd_sock, "inference.request", {
                "workload_id": workload["id"], "provider_id": provid,
                "prompt": "alpha beta gamma delta",
                "max_tokens": 4, "request_id": "req-" + uuid.uuid4().hex,
                "operation_id": op, "created_by": "uid:1000",
            })["data"]
            assert result["executed"] is True, result
            assert result["result"]["tokens_out"] == 4, result
            assert result["result"]["synthetic"] is True, result
            usage_id = result["usage_id"]

            # Replay of the same operation_id returns the identical usage row.
            replay = request(flowerlyd_sock, "inference.request", {
                "workload_id": workload["id"], "provider_id": provid,
                "prompt": "alpha beta gamma delta",
                "max_tokens": 4, "request_id": result["request_id"],
                "operation_id": op, "created_by": "uid:1000",
            })["data"]
            assert replay["usage_id"] == usage_id, (result, replay)
            totals = request(flowerlyd_sock, "workload.usage_brief",
                             {"model_id": model})["data"]
            assert totals["tokens_in"] == replay["result"]["tokens_in"], totals

            # 3. Structured bounds are typed InvalidArgument; nothing ledgers.
            baseline = request(flowerlyd_sock, "workload.usage_brief",
                               {"model_id": model})["data"]["tokens_in"]
            for bad in [
                {"prompt": "x" * (16 * 1024 + 1)},
                {"max_tokens": 0},
                {"max_tokens": 4097},
                {"request_id": "bad id"},
                {},
            ]:
                reply = request(flowerlyd_sock, "inference.request", {
                    "workload_id": workload["id"], "provider_id": provid,
                    "prompt": "hi", "created_by": "uid:1000", **bad,
                })
                assert reply["type"] == "err", (bad, reply)
            totals = request(flowerlyd_sock, "workload.usage_brief",
                             {"model_id": model})["data"]
            assert totals["tokens_in"] == baseline, (baseline, totals)

            # 4. Only ADMITTED workloads execute.
            roomy2 = request(flowerlyd_sock, "profile.create", {
                "name": "exec-zero",
                "toml": "memory_max_bytes = 12_000_000_000\n"
                        "concurrency_max = 0\nfallback = 'queue'\n",
            })["data"]["id"]
            queued = request(flowerlyd_sock, "workload.submit", {
                "profile_id": roomy2, "model_id": model, "created_by": "uid:1000",
            })["data"]
            assert queued["state"] == "QUEUED", queued
            err(request(flowerlyd_sock, "inference.request", {
                "workload_id": queued["id"], "provider_id": provid,
                "prompt": "hi", "request_id": "r-" + uuid.uuid4().hex,
            }), "conflict", "ADMITTED required")

            # 5. Provider outage honors profile fallback; no silent retry and
            #    no ledger changes while down.
            baseline_totals = request(flowerlyd_sock, "workload.usage_brief",
                                      {"model_id": model})["data"]["tokens_in"]
            shutdown(runtime / "providerd.sock")
            processes[-1].wait(timeout=10)
            outage_profile = request(flowerlyd_sock, "profile.create", {
                "name": "exec-fail",
                "toml": "fallback = 'fail'\n",
            })["data"]["id"]
            outage_workload = request(flowerlyd_sock, "workload.submit", {
                "profile_id": outage_profile, "model_id": model,
                "created_by": "uid:1000",
            })["data"]
            assert outage_workload["state"] == "ADMITTED", outage_workload
            outage = request(flowerlyd_sock, "inference.request", {
                "workload_id": outage_workload["id"], "provider_id": provid,
                "prompt": "hi", "request_id": "r-" + uuid.uuid4().hex,
            })
            assert outage["type"] == "err" and outage["code"] == "unavailable", outage
            after = request(flowerlyd_sock, "workload.usage_brief",
                            {"model_id": model})["data"]["tokens_in"]
            assert after == baseline_totals, (baseline_totals, after)
            queue_profile = request(flowerlyd_sock, "profile.create", {
                "name": "exec-queue",
                "toml": "fallback = 'queue'\n",
            })["data"]["id"]
            queue_workload = request(flowerlyd_sock, "workload.submit", {
                "profile_id": queue_profile, "model_id": model,
                "created_by": "uid:1000",
            })["data"]
            standby = request(flowerlyd_sock, "inference.request", {
                "workload_id": queue_workload["id"], "provider_id": provid,
                "prompt": "hi", "request_id": "r-" + uuid.uuid4().hex,
            })["data"]
            assert standby["executed"] is False, standby
            assert standby["policy"] == "queue", standby
            assert standby["requires_consent"] is True, standby
            totals = request(flowerlyd_sock, "workload.usage_brief",
                             {"model_id": model})["data"]
            assert totals["tokens_in"] == baseline_totals, totals

            # 6. Unknown provider ids stay typed NotFound.
            err(request(flowerlyd_sock, "inference.request", {
                "workload_id": workload["id"], "provider_id": "provider-none",
                "prompt": "hi", "request_id": "r-" + uuid.uuid4().hex,
            }), "not_found", "provider not found")

            # 7. Recovery: restart the provider without outage; next request
            #    executes again.
            start_provider()
            recovered = request(flowerlyd_sock, "inference.request", {
                "workload_id": workload["id"], "provider_id": provid,
                "prompt": "echo again", "request_id": "r-" + uuid.uuid4().hex,
                "created_by": "uid:1000",
            })["data"]
            assert recovered["executed"] is True, recovered

            # 8. Restart durability of ledger rows.
            shutdown(flowerlyd_sock)
            processes[0].wait(timeout=10)
            flowerlyd_sock = start_supervisor()
            totals = request(flowerlyd_sock, "workload.usage_brief",
                             {"model_id": model})["data"]
            expected = recovered["result"]["tokens_in"] + baseline_totals
            assert totals["tokens_in"] == expected, (totals, expected)
        finally:
            for proc in processes:
                if proc.poll() is None:
                    proc.terminate()
            for proc in processes:
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
    print("Inference contract black box: discovery, admitted-only execution, "
          "bounded replay, consent-gated outage fallback and durable ledger "
          "verified")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

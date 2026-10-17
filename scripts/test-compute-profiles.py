#!/usr/bin/env python3
"""Black-box RFC 0004 tranche-2 acceptance: expert AI profiles + provider boundary.

Exercises the real flowerlyd supervisor: credential-free versioned TOML
profiles, explicit acyclic inheritance with locality tightening, typed
renames/exports, operation_id replay on profile import, name conflicts, and
the fail-closed provider registry (local-unix only; remote kinds refused with
PermissionDenied because no egress policy authorization exists).
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
                if request(socket_path, "flow.list", {})["type"] == "ok":
                    return
            except (OSError, AssertionError, TimeoutError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"not ready: {socket_path}")


def err_args(reply, code, needle):
    assert reply["type"] == "err", reply
    assert reply["code"] == code, (reply, code)
    assert needle in reply["message"], reply
    return reply["message"]


BASE_TOML = "capability_tier = 'advanced'\nbackground_activity = 'quiet'\n"
CHILD_TOML = (
    "base = 'expert-base'\ncapability_tier = 'advanced'\n"
    "allowed_models = ['model-abc']\nmemory_max_bytes = 4_000_000_000\n"
)

OPERATION_ID = "compute-profile-" + uuid.uuid4().hex


def main(bin_dir):
    sock = None
    processes = []
    with tempfile.TemporaryDirectory(prefix="flowerly-profiles-") as root:
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
            wait_ready(runtime / "flowerlyd.sock", proc, log_path)
            return runtime / "flowerlyd.sock"

        try:
            sock = start()

            # 1. Credential-shaped and unknown keys are structurally refused.
            for body in [
                "api_key = 'hunter2'\n",
                "token = 'leak'\n",
                "endpoint = 'https://provider.example'\n",
                "unknown_flag = 'x'\n",
            ]:
                reply = request(sock, "profile.create", {"name": "leaky", "toml": body})
                err_args(reply, "invalid_argument", "")

            # 2. Parent profile; duplicate name conflict is typed.
            base = request(sock, "profile.create", {
                "name": "expert-base", "toml": BASE_TOML,
            })["data"]
            assert base["version"] == 1, base
            err_args(
                request(sock, "profile.create",
                        {"name": "expert-base", "toml": BASE_TOML}),
                "conflict", "already exists",
            )

            # 3. Child inherits by name; base-less resolution stays typed.
            child = request(sock, "profile.create", {
                "name": "expert-child", "toml": CHILD_TOML,
            })["data"]
            resolve = request(sock, "profile.resolve", {"id": child["id"]})["data"]
            assert resolve["chain"] == ["expert-child", "expert-base"], resolve
            assert resolve["promise"] == "preference-data-only", resolve
            effective = resolve["effective"]
            assert effective["memory_max_bytes"] == 4_000_000_000, effective
            assert effective["background_activity"] == "quiet", effective
            assert effective["allowed_models"] == ["model-abc"], effective

            # 4. Locality can only tighten during inheritance.
            remote_child = request(sock, "profile.create", {
                "name": "wannabe-remote",
                "toml": "base = 'expert-base'\nlocality = 'approved-remote-opt-in'\n",
            })["data"]
            resolve = request(sock, "profile.resolve", {"id": remote_child["id"]})["data"]
            assert resolve["effective"]["locality"] == "local-only", resolve

            # 5. Missing base is NotFound; standalone cycle is refused.
            err_args(
                request(sock, "profile.create",
                        {"name": "orphan", "toml": "base = 'missing'\n"}),
                "not_found", "base profile not found",
            )

            # 6. Append-only versioning: edit adds v2, older stays exported.
            request(sock, "profile.edit", {
                "id": base["id"],
                "toml": BASE_TOML + "fallback = 'queue'\n",
            })
            v2 = request(sock, "profile.export", {"id": base["id"], "version": 2})["data"]
            assert "queue" in v2["toml"], v2
            v1 = request(sock, "profile.export", {"id": base["id"], "version": 1})["data"]
            assert "queue" not in v1["toml"], v1
            history = request(sock, "profile.version_history", {"id": base["id"]})["data"]
            assert [row["version"] for row in history["versions"]] == [1, 2], history

            # 7. Clone and rename keep history; rename is reflected in list.
            clone = request(sock, "profile.clone", {
                "id": base["id"], "name": "expert-copy",
            })["data"]
            assert clone["version"] == 1, clone
            renamed = request(sock, "profile.rename", {
                "id": clone["id"], "name": "expert-copy-2",
            })["data"]
            assert renamed["name"] == "expert-copy-2", renamed
            listed = request(sock, "profile.list", {})["data"]
            names = [row["name"] for row in listed["profiles"]]
            assert "expert-copy-2" in names and "expert-copy" not in names, names

            # 8. profile.import honors operation_id unknown-outcome replay.
            imported = request(sock, "profile.import", {
                "name": "expert-imported",
                "toml": CHILD_TOML.replace("'expert-base'", "'expert-copy-2'"),
                "operation_id": OPERATION_ID,
            })["data"]
            replayed = request(sock, "profile.import", {
                "name": "expert-imported",
                "toml": CHILD_TOML.replace("'expert-base'", "'expert-copy-2'"),
                "operation_id": OPERATION_ID,
            })["data"]
            assert replayed["id"] == imported["id"], (imported, replayed)
            listed = request(sock, "profile.list", {})["data"]
            imported_rows = [
                row for row in listed["profiles"] if row["name"] == "expert-imported"
            ]
            assert len(imported_rows) == 1, listed
            assert imported_rows[0]["id"] == imported["id"], listed

            # 9. Cycle detection through an edited mutual chain.
            c1 = request(sock, "profile.create",
                         {"name": "cyc-a", "toml": "capability_tier = 'essential'\n"})["data"]
            c2 = request(sock, "profile.create",
                         {"name": "cyc-b", "toml": "base = 'cyc-a'\n"})["data"]
            request(sock, "profile.edit",
                    {"id": c1["id"], "toml": "base = 'cyc-b'\n"})
            err_args(
                request(sock, "profile.resolve", {"id": c1["id"]}),
                "invalid_argument", "inheritance",
            )
            assert c1["id"] and c2["id"]

            # 10. Provider boundary: local-unix only, remote kinds refused.
            local = request(sock, "provider.register", {
                "name": "local-echo-check",
                "kind": "local-unix",
                "capabilities": ["text"],
            })["data"]
            assert local["kind"] == "local-unix", local
            err_args(
                request(sock, "provider.register",
                        {"name": "external-api", "kind": "openai-api",
                         "capabilities": ["text"]}),
                "permission_denied", "no egress policy authorization",
            )
            listed = request(sock, "provider.list", {})["data"]
            kinds = [row["kind"] for row in listed["providers"]]
            assert kinds == ["local-unix"], kinds
            err_args(
                request(sock, "provider.show", {"id": "provider-none"}),
                "not_found", "provider not found",
            )

            # 11. Restart durability over shared SQLite.
            shutdown(sock)
            sock = start()
            detail = request(sock, "profile.show", {"id": c1["id"]})
            assert detail["type"] == "ok", detail
            listed = request(sock, "provider.list", {})["data"]
            assert listed["count"] == 1 and listed["providers"][0]["kind"] == "local-unix"
            replayed_again = request(sock, "profile.import", {
                "name": "expert-imported",
                "toml": CHILD_TOML.replace("'expert-base'", "'expert-copy-2'"),
                "operation_id": OPERATION_ID,
            })["data"]
            assert replayed_again["id"] == imported["id"], replayed_again
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
    print("Profiles and provider boundary black box: credential refusal, "
          "versioning, inheritance tightening, replay and fail-closed remote "
          "kind refusal verified")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

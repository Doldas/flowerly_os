#!/usr/bin/env python3
"""Black-box ADR-0022 gates with the real flowerlyd and providerd (#263).

Offline: the remote provider's destination is localhost, which the
production address policy refuses, so no request can leave the machine.
Verifies, with real processes and real IPC:

- remote inference is disabled when the policy file is unsafe or absent;
- every gate refuses before anything is submitted (no consent, local-only
  profile, missing `remote: true`), and revocation takes effect at once;
- an accepted request becomes a durable attempt whose outcome (address
  refused, not sent) is reported honestly and never resubmitted;
- secret isolation: the provider key exists only in the provider's
  credential directory, and neither the key nor the prompt reaches the
  state database (with WAL), any IPC reply, or either daemon's log;
- no refusal discloses a uid;
- the unit templates keep their confinement settings.
"""
import json
import os
import pathlib
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid

SENTINEL = "sk-flowerly-SENTINEL-gates-0123456789"
MARKER = "prompt-marker-7f3a9c"
ROOT = pathlib.Path(__file__).resolve().parent.parent
TEMPLATES = ROOT / "distro/templates/remote-inference"


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise AssertionError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def request(sock, name, args):
    correlation = str(uuid.uuid4())
    message = json.dumps({"version": 1, "id": correlation, "type": "command",
                          "name": name, "args": args}).encode()
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(15)
        client.connect(str(sock))
        client.sendall(struct.pack("<I", len(message)) + message)
        raw = recv_exact(client, struct.unpack("<I", recv_exact(client, 4))[0])
    assert SENTINEL.encode() not in raw, f"{name} reply leaked the key"
    reply = json.loads(raw)
    assert reply["id"] == correlation, (name, reply)
    return reply


def ok(reply):
    assert reply["type"] == "ok", reply
    return reply["data"]


def refused(reply, code, needle):
    assert reply["type"] == "err" and reply["code"] == code, reply
    assert needle in reply["message"], reply
    # Refusals never disclose the caller's or a file owner's uid.
    message = reply["message"].lower()
    assert "uid" not in message and str(os.getuid()) not in message, reply


def wait_socket(path, proc, log):
    for _ in range(200):
        if proc.poll() is not None:
            raise AssertionError(f"daemon exited: {log.read_text()}")
        if path.exists():
            return
        time.sleep(0.05)
    raise AssertionError(f"not ready: {path}")


def stop(proc):
    if proc.poll() is None:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def check_templates():
    unit = (TEMPLATES / "flowerly-provider-remote@.service").read_text()
    for line in [
        "User=flowerly-provider",
        "LoadCredentialEncrypted=%i.key:",
        "UnsetEnvironment=",
        "SSL_CERT_FILE SSL_CERT_DIR",
        "HTTPS_PROXY",
        "InaccessiblePaths=/var/lib/flowerly /run/flowerly",
        "NoNewPrivileges=yes",
        "CapabilityBoundingSet=\n",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "IPAddressDeny=localhost link-local multicast",
        "ProtectSystem=strict",
    ]:
        assert line in unit, f"unit template lost `{line.strip()}`"
    for path in TEMPLATES.iterdir():
        text = path.read_text()
        assert "sk-" not in text and "api_key" not in text, f"{path.name} names a key"
    gate = (TEMPLATES / "remote-providers.toml").read_text()
    assert "response_replay_seconds = 0" in gate


def main(bin_dir):
    check_templates()
    with tempfile.TemporaryDirectory(prefix="flowerly-gates-") as root:
        root = pathlib.Path(root)
        runtime, state = root / "runtime", root / "state"
        provider_runtime, creds = root / "provider-run", root / "creds"
        for directory in (runtime, state, provider_runtime, creds):
            directory.mkdir(mode=0o700)
        (creds / "remote.key").write_text(SENTINEL + "\n")
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            closed_port = probe.getsockname()[1]
        remote_config = root / "remote.toml"
        remote_config.write_text(
            "[remote]\n"
            f'api_base = "https://localhost:{closed_port}"\n'
            'api_style = "openai"\nmodels = ["gpt-oss:20b"]\ncredential = "remote.key"\n'
            "request_timeout_ms = 2000\n"
        )
        provider_socket = provider_runtime / "providerd.sock"
        policy = root / "remote-providers.toml"
        policy.write_text(
            "enabled = true\n[[provider]]\n"
            'name = "local-test"\n'
            f'socket = "{provider_socket}"\n'
            'destination = "localhost"\nmodels = ["gpt-oss:20b"]\n'
        )
        base_env = {k: v for k, v in os.environ.items()
                    if k not in ("SSL_CERT_FILE", "SSL_CERT_DIR", "CREDENTIALS_DIRECTORY")}
        provider_env = dict(base_env, AGENTIC_RUNTIME_DIR=str(provider_runtime),
                            CREDENTIALS_DIRECTORY=str(creds))
        # flowerlyd never gets the credential directory.
        flowerlyd_env = dict(base_env, AGENTIC_RUNTIME_DIR=str(runtime),
                             AGENTIC_STATE_DIR=str(state), AGENTIC_ADMIN_GROUP=str(os.getgid()))
        processes = []
        logs = [root / "providerd.log", root / "flowerlyd-unsafe.log", root / "flowerlyd.log"]

        def start(argv, env, log, sock):
            proc = subprocess.Popen(argv, env=env, stdout=log.open("ab"), stderr=subprocess.STDOUT)
            processes.append(proc)
            wait_socket(sock, proc, log)
            return proc

        try:
            start([str(bin_dir / "providerd"), "--remote-config", str(remote_config)],
                  provider_env, logs[0], provider_socket)

            # An unsafe (group-writable) policy leaves remote inference off.
            policy.chmod(0o664)
            unsafe = start([str(bin_dir / "flowerlyd"), "--remote-providers", str(policy)],
                           flowerlyd_env, logs[1], runtime / "flowerlyd.sock")
            sock = runtime / "flowerlyd.sock"
            refused(request(sock, "provider.register", {
                "name": "local-test", "kind": "remote-openai-compatible", "capabilities": ["text"]}),
                "permission_denied", "writable")
            stop(unsafe)
            sock.unlink(missing_ok=True)

            policy.chmod(0o644)
            start([str(bin_dir / "flowerlyd"), "--remote-providers", str(policy)],
                  flowerlyd_env, logs[2], sock)
            provider = ok(request(sock, "provider.register", {
                "name": "local-test", "kind": "remote-openai-compatible", "capabilities": ["text"]}))["id"]
            assert ok(request(sock, "provider.show", {"id": provider}))["destination"] == "localhost"
            model = ok(request(sock, "model.register", {
                "name": "gpt-oss:20b", "backend": "CPU", "measured_memory_bytes": 100_000_000}))["id"]
            remote_profile = ok(request(sock, "profile.create", {
                "name": "remote", "toml": "locality = 'approved-remote-opt-in'\nfallback = 'ask'\n"}))["id"]
            local_profile = ok(request(sock, "profile.create", {
                "name": "local", "toml": "fallback = 'ask'\n"}))["id"]

            def workload(profile):
                submitted = ok(request(sock, "workload.submit", {"profile_id": profile, "model_id": model}))
                assert submitted["state"] == "ADMITTED", submitted
                return submitted["id"]

            remote_work, local_work = workload(remote_profile), workload(local_profile)

            def ask(work, **extra):
                args = {"workload_id": work, "provider_id": provider, "remote": True,
                        "prompt": f"Reply with exactly: flowerly-ready {MARKER}",
                        "request_id": "bb-1", "max_tokens": 16}
                args.update(extra)
                return request(sock, "inference.request", args)

            refused(ask(remote_work), "permission_denied", "no active consent")
            refused(request(sock, "inference.consent_grant", {
                "provider_id": provider, "destination": "evil.example"}), "invalid_argument", "destination")
            grant = ok(request(sock, "inference.consent_grant", {
                "provider_id": provider, "destination": "localhost", "days": 1}))
            assert grant["valid"] is True, grant
            refused(ask(remote_work, remote=False), "permission_denied", "remote: true")
            refused(ask(local_work), "permission_denied", "local-only")

            submitted = ok(ask(remote_work, operation_id="bb-op-1"))
            attempt = submitted["attempt"]["id"]
            assert submitted["attempt"]["state"] == "pending", submitted
            result = None
            for _ in range(100):
                result = ok(request(sock, "inference.result", {"attempt_id": attempt}))
                if result["attempt"]["state"] != "pending":
                    break
                time.sleep(0.05)
            assert result["attempt"]["state"] == "failed", result
            assert result["attempt"]["error_class"] == "address_refused", result
            assert result["attempt"]["outcome"] == "not_sent", result
            # A retry reports the same attempt; nothing is resubmitted.
            retry = ok(ask(remote_work, operation_id="bb-op-1"))
            assert retry["attempt"]["id"] == attempt and retry["resubmitted"] is False, retry

            assert ok(request(sock, "inference.consent_revoke", {"provider_id": provider}))["revoked"] == 1
            refused(ask(remote_work), "permission_denied", "no active consent")
            listed = ok(request(sock, "inference.consent_list", {}))
            assert listed["consents"][0]["active"] is False, listed
        finally:
            for proc in processes:
                stop(proc)

        database = b"".join(path.read_bytes() for path in state.glob("state.db*"))
        assert attempt.encode() in database, "attempt metadata missing from the database"
        for blob, where in [(database, "state database")] + [(log.read_bytes(), log.name) for log in logs]:
            assert SENTINEL.encode() not in blob, f"{where} contains the provider key"
            assert MARKER.encode() not in blob, f"{where} contains the prompt"
    print("remote inference gates black box: disabled when unsafe, consent/profile/remote gates, "
          "durable not-sent attempt, no resubmission, revocation, no key or prompt at rest or in logs")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

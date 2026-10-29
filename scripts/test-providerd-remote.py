#!/usr/bin/env python3
"""Black-box checks of the real providerd binary's opt-in remote backend (#263).

Offline: no real server, no real key, no network use. Verifies that the
backend fails closed at startup (missing credential, non-HTTPS origin,
environment trust-store override, unknown config fields), that a served
backend refuses the production address policy's loopback destination with
a typed not_sent outcome, that the synchronous echo command is refused,
and that the sentinel key never appears in any output, IPC reply or log.
TLS, HTTP status, limit and outcome semantics are covered in-process by
`cargo test -p providerd` against a local HTTPS mock server.
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

SENTINEL = "sk-flowerly-SENTINEL-blackbox-0123456789"


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
        "version": 1, "id": correlation, "type": "command", "name": name, "args": args,
    }).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(15)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        raw = recv_exact(client, size)
    assert SENTINEL.encode() not in raw, f"{name} reply leaked the key"
    reply = json.loads(raw)
    assert reply["id"] == correlation, (name, reply)
    return reply


def shutdown(socket_path):
    message = json.dumps({"version": 1, "id": str(uuid.uuid4()), "type": "shutdown"}).encode()
    try:
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(8)
            client.connect(str(socket_path))
            client.sendall(struct.pack("<I", len(message)) + message)
            recv_exact(client, struct.unpack("<I", recv_exact(client, 4))[0])
    except OSError:
        pass


def config_text(api_base, extra=""):
    return (
        "[remote]\n"
        f'api_base = "{api_base}"\n'
        'api_style = "openai"\n'
        'models = ["gpt-oss:20b"]\n'
        'credential = "remote.key"\n'
        "request_timeout_ms = 2000\n"
        f"{extra}"
    )


def main(bin_dir):
    providerd = str(bin_dir / "providerd")
    with tempfile.TemporaryDirectory(prefix="flowerly-remote-") as root:
        root = pathlib.Path(root)
        runtime, creds = root / "runtime", root / "creds"
        runtime.mkdir(mode=0o700)
        creds.mkdir(mode=0o700)
        (creds / "remote.key").write_text(SENTINEL + "\n")
        (creds / "remote.key").chmod(0o600)
        # A port nothing listens on; the address policy refuses it first anyway.
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            closed_port = probe.getsockname()[1]
        base = f"https://localhost:{closed_port}"
        clean_env = {key: value for key, value in os.environ.items()
                     if key not in ("SSL_CERT_FILE", "SSL_CERT_DIR", "CREDENTIALS_DIRECTORY")}
        env = dict(clean_env, AGENTIC_RUNTIME_DIR=str(runtime), CREDENTIALS_DIRECTORY=str(creds))

        def self_test(config, env_row):
            path = root / "remote.toml"
            path.write_text(config)
            result = subprocess.run(
                [providerd, "--remote-config", str(path), "--self-test"],
                env=env_row, capture_output=True, text=True, timeout=30,
            )
            output = result.stdout + result.stderr
            assert SENTINEL not in output, "self-test output leaked the key"
            return result.returncode, output

        code, output = self_test(config_text(base), env)
        assert code == 0 and "no network request made" in output, output

        for case, config, env_row, expected in [
            ("missing credential directory", config_text(base),
             {k: v for k, v in env.items() if k != "CREDENTIALS_DIRECTORY"}, "LoadCredential"),
            ("plain http", config_text(f"http://localhost:{closed_port}"), env, "https"),
            ("unknown field", config_text(base, "verify_tls = false\n"), env, "unknown field"),
            ("trust override", config_text(base), dict(env, SSL_CERT_FILE=str(root / "ca.pem")),
             "SSL_CERT_FILE"),
        ]:
            code, output = self_test(config, env_row)
            assert code != 0, f"{case}: started"
            assert expected in output, f"{case}: {output}"

        # Serve with the production address policy: loopback is refused.
        config = root / "remote.toml"
        config.write_text(config_text(base))
        log = root / "providerd.log"
        proc = subprocess.Popen(
            [providerd, "--remote-config", str(config)], env=env,
            stdout=log.open("ab"), stderr=subprocess.STDOUT,
        )
        sock = runtime / "providerd.sock"
        try:
            for _ in range(200):
                if proc.poll() is not None:
                    raise AssertionError(f"providerd exited: {log.read_text()}")
                if sock.exists():
                    break
                time.sleep(0.05)
            discovered = request(sock, "provider.discover", {})
            assert discovered["type"] == "err" and discovered["code"] == "permission_denied", discovered
            assert "address_refused" in discovered["message"], discovered

            generate = request(sock, "provider.generate", {"request_id": "r", "prompt": "hi"})
            assert generate["type"] == "err" and generate["code"] == "invalid_argument", generate

            submitted = request(sock, "provider.submit", {
                "request_id": "bb-1", "prompt": "Reply with exactly: flowerly-ready",
                "max_tokens": 16, "created_by": "uid:1000",
            })
            assert submitted["type"] == "ok" and submitted["data"]["state"] == "pending", submitted
            status = None
            for _ in range(200):
                status = request(sock, "provider.poll", {"request_id": "bb-1", "created_by": "uid:1000"})
                if status["data"]["state"] != "pending":
                    break
                time.sleep(0.05)
            data = status["data"]
            assert data["state"] == "failed", data
            assert data["error"]["class"] == "address_refused", data
            assert data["error"]["outcome"] == "not_sent", data
        finally:
            shutdown(sock)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        assert SENTINEL not in log.read_text(), "providerd log leaked the key"
    print("providerd remote backend black box: fail-closed startup, address policy, typed outcomes, no key leakage")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

#!/usr/bin/env python3
"""Offline contract tests for the remote provider operator procedure (#263).

Nothing here contacts a real server, needs a key or needs root:

- the unit template keeps its confinement, verifies with systemd-analyze
  and stays within an exposure threshold;
- `render` writes exactly the documented files, refuses unsafe inputs,
  keeps keys out of every file, and allows a loopback DNS stub only;
- the rendered remote config is accepted by `providerd --self-test`;
- `providerd --discover-only` refuses every prompt before anything is sent;
- the acceptance client sends at most one prompt, never after a
  discovery-only or missing-model answer, and never prints the text;
- the evidence writer refuses to write evidence that would leak;
- DNS classification admits public unicast addresses only;
- every privileged subcommand refuses to run without root.
"""
import importlib.util
import json
import os
import pathlib
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
OPERATOR = ROOT / "scripts/remote-provider-operator.sh"
CLIENT = ROOT / "scripts/remote-provider-acceptance.py"
UNIT = ROOT / "distro/templates/remote-inference/flowerly-provider-remote@.service"
INSTANCE, API_BASE, STYLE, MODEL = "gamitron", "https://openui.gamitronservers.com", "open-webui", "gpt-oss:20b"
EXPOSURE_THRESHOLD = 20  # systemd-analyze security, tenths: 2.0

spec = importlib.util.spec_from_file_location("acceptance", CLIENT)
acceptance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(acceptance)


def run(argv, env=None, check=True):
    result = subprocess.run(argv, capture_output=True, text=True, env=env, timeout=60)
    if check and result.returncode != 0:
        raise AssertionError(f"{argv} failed: {result.stdout}{result.stderr}")
    return result


def render(dest, resolv, *args):
    env = dict(os.environ, RESOLV_CONF=str(resolv))
    return run(["bash", str(OPERATOR), "render", *args, str(dest)], env=env, check=False)


def check_unit_template():
    unit = UNIT.read_text()
    for line in [
        "User=flowerly-provider\n",
        "Group=flowerly-provider-clients\n",
        "LoadCredentialEncrypted=%i.key:/etc/credstore.encrypted/%i.key\n",
        "SSL_CERT_FILE SSL_CERT_DIR",
        "HTTPS_PROXY",
        "InaccessiblePaths=/var/lib/flowerly /run/flowerly /etc/credstore /etc/credstore.encrypted\n",
        "NoNewPrivileges=yes\n",
        "CapabilityBoundingSet=\n",
        "AmbientCapabilities=\n",
        "ProtectSystem=strict\n",
        "ProtectHome=yes\n",
        "PrivateTmp=yes\n",
        "PrivateDevices=yes\n",
        "ProtectProc=invisible\n",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6\n",
        "RestrictNamespaces=yes\n",
        "MemoryDenyWriteExecute=yes\n",
        "SystemCallFilter=@system-service\n",
        "IPAddressDeny=localhost link-local multicast\n",
    ]:
        assert line in unit, f"unit template lost `{line.strip()}`"
    for forbidden in ("Environment=", "LoadCredential=", "PassEnvironment=", "--discover-only"):
        assert forbidden not in unit.replace("UnsetEnvironment=", ""), f"unit template has {forbidden}"


def check_render(scratch, bin_dir):
    stub = scratch / "resolv-stub"
    stub.write_text("nameserver 127.0.0.53\noptions edns0\n")
    lan = scratch / "resolv-lan"
    lan.write_text("nameserver 192.168.0.1\nnameserver ::1\n")

    dest = scratch / "root"
    result = render(dest, stub, INSTANCE, API_BASE, STYLE, MODEL)
    assert result.returncode == 0, result.stderr
    files = sorted(str(p.relative_to(dest)) for p in dest.rglob("*") if p.is_file())
    assert files == sorted([
        "etc/flowerly/providers/gamitron.service.toml",
        "etc/flowerly/providers/gamitron.toml",
        "etc/systemd/system/flowerly-provider-remote@.service",
        "etc/systemd/system/flowerly-provider-remote@gamitron.service.d/10-operator.conf",
        "etc/sysusers.d/flowerly-provider.conf",
    ]), files
    for path in dest.rglob("*"):
        if path.is_file():
            text = path.read_text()
            assert "api_key" not in text and "Bearer" not in text, path
            assert oct(path.stat().st_mode & 0o777) == "0o644", path
    dropin = (dest / "etc/systemd/system/flowerly-provider-remote@gamitron.service.d/10-operator.conf").read_text()
    assert "IPAddressAllow=127.0.0.53\n" in dropin, dropin
    assert "--discover-only" not in dropin
    sysusers = (dest / "etc/sysusers.d/flowerly-provider.conf").read_text()
    assert "\nm " not in sysusers and "u flowerly-provider " in sysusers, sysusers
    config = (dest / "etc/flowerly/providers/gamitron.toml").read_text()
    assert 'api_style = "open-webui"' in config and 'credential = "gamitron.key"' in config

    # A LAN resolver is reachable anyway; ::1 is a loopback stub.
    lan_dest = scratch / "root-lan"
    assert render(lan_dest, lan, INSTANCE, API_BASE, STYLE, MODEL).returncode == 0
    lan_dropin = (lan_dest / "etc/systemd/system/flowerly-provider-remote@gamitron.service.d/10-operator.conf").read_text()
    assert "IPAddressAllow=::1\n" in lan_dropin and "192.168" not in lan_dropin, lan_dropin

    refused = [
        ("Gamitron", API_BASE, STYLE, MODEL),
        ("../x", API_BASE, STYLE, MODEL),
        (INSTANCE, "http://openui.gamitronservers.com", STYLE, MODEL),
        (INSTANCE, "https://user@openui.gamitronservers.com", STYLE, MODEL),
        (INSTANCE, "https://openui.gamitronservers.com/api", STYLE, MODEL),
        (INSTANCE, "https://openui.gamitronservers.com?x=1", STYLE, MODEL),
        (INSTANCE, API_BASE, "ollama", MODEL),
        (INSTANCE, API_BASE, STYLE, 'gpt"]\nx = ["'),
    ]
    for index, args in enumerate(refused):
        target = scratch / f"refused-{index}"
        result = render(target, stub, *args)
        assert result.returncode != 0, args
        assert not any(p.is_file() for p in target.rglob("*")), f"{args} wrote files"

    # The rendered remote config is accepted, with no network request.
    creds = scratch / "creds"
    creds.mkdir(mode=0o700)
    (creds / "gamitron.key").write_text("sk-offline-dummy-not-a-key\n")
    env = {k: v for k, v in os.environ.items() if k not in ("SSL_CERT_FILE", "SSL_CERT_DIR")}
    env["CREDENTIALS_DIRECTORY"] = str(creds)
    result = run([str(bin_dir / "providerd"), "--remote-config",
                  str(dest / "etc/flowerly/providers/gamitron.toml"), "--self-test"], env=env)
    assert "no network request made" in result.stdout, result.stdout
    return dest


def check_systemd(dest, bin_dir):
    analyze = shutil.which("systemd-analyze")
    if analyze is None:
        print("systemd-analyze unavailable: unit verification skipped")
        return
    unit_dir = dest / "etc/systemd/system/flowerly-provider-remote@gamitron.service.d"
    # Point ExecStart at the built binary so verify checks the real unit.
    (unit_dir / "99-test.conf").write_text(
        f"[Service]\nExecStart=\nExecStart={bin_dir.resolve() / 'providerd'} --remote-config /dev/null\n")
    env = dict(os.environ, SYSTEMD_UNIT_PATH=f"{dest / 'etc/systemd/system'}:")
    result = run([analyze, "verify", "--man=no", "flowerly-provider-remote@gamitron.service"],
                 env=env, check=False)
    ours = [line for line in (result.stdout + result.stderr).splitlines()
            if "flowerly-provider-remote" in line]
    assert not ours, f"systemd-analyze verify: {ours}"
    (unit_dir / "99-test.conf").unlink()
    result = run([analyze, "security", "--no-pager", "--offline=yes",
                  f"--threshold={EXPOSURE_THRESHOLD}", str(UNIT)], check=False)
    assert result.returncode == 0, f"exposure above {EXPOSURE_THRESHOLD / 10}: {result.stdout[-400:]}"
    print(result.stdout.strip().splitlines()[-1])


def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data.extend(chunk)
    return bytes(data)


class FakeProvider:
    """A providerd stand-in on a Unix socket, scripted per command."""

    def __init__(self, path, discover):
        self.path, self.discover, self.commands = str(path), discover, []
        self.polls = 0
        self.server = socket.socket(socket.AF_UNIX)
        self.server.bind(self.path)
        self.server.listen()
        threading.Thread(target=self.serve, daemon=True).start()

    def reply(self, name, args):
        if name == "provider.discover":
            return {"type": "ok", "data": self.discover}
        if name == "provider.submit":
            return {"type": "ok", "data": {"request_id": args["request_id"], "state": "pending", "sent": False}}
        self.polls += 1
        if self.polls == 1:
            return {"type": "ok", "data": {"state": "pending", "sent": True}}
        result = {"destination": "openui.gamitronservers.com", "model_requested": MODEL,
                  "model_used": MODEL, "truncated": False, "finish_reason": "stop",
                  "usage": {"source": "provider_reported", "tokens_in": 70, "tokens_out": 9},
                  "latency_ms": 812}
        if self.polls == 2:
            result["text"] = "flowerly-ready"
        else:
            result["text_released"] = True
        return {"type": "ok", "data": {"state": "succeeded", "result": result}}

    def serve(self):
        while True:
            try:
                conn, _ = self.server.accept()
            except OSError:
                return
            with conn:
                message = json.loads(recv_exact(conn, struct.unpack("<I", recv_exact(conn, 4))[0]))
                self.commands.append(message["name"])
                body = dict(self.reply(message["name"], message["args"]), id=message["id"], version=1)
                raw = json.dumps(body).encode()
                conn.sendall(struct.pack("<I", len(raw)) + raw)

    def close(self):
        self.server.close()


def client(*args):
    return run([sys.executable, "-I", str(CLIENT), *args], check=False)


def check_client(scratch):
    served = {"destination": "openui.gamitronservers.com", "api_style": STYLE,
              "models_allowlisted": [MODEL], "models_available": [MODEL], "discover_only": False}

    fake = FakeProvider(scratch / "ok.sock", served)
    result = client("accept", fake.path)
    fake.close()
    report = json.loads(result.stdout)
    assert result.returncode == 0 and report["ok"] is True, result.stdout
    assert report["text_matches_expected"] is True and report["text_length"] == 14, report
    assert report["text_released_after_delivery"] is True, report
    assert "flowerly-ready" not in result.stdout, "the client printed the generated text"
    assert fake.commands.count("provider.submit") == 1, fake.commands

    for name, discover in [
        ("preflight", dict(served, discover_only=True)),
        ("absent", dict(served, models_available=[])),
    ]:
        fake = FakeProvider(scratch / f"{name}.sock", discover)
        result = client("accept", fake.path)
        fake.close()
        report = json.loads(result.stdout)
        assert result.returncode == 1 and report["prompt_sent"] is False, (name, result.stdout)
        assert "provider.submit" not in fake.commands, (name, fake.commands)

    fake = FakeProvider(scratch / "discover.sock", served)
    result = client("discover", fake.path)
    fake.close()
    assert result.returncode == 0 and fake.commands == ["provider.discover"], fake.commands


def check_discover_only(scratch, bin_dir):
    runtime = scratch / "run"
    creds = scratch / "creds-do"
    for directory in (runtime, creds):
        directory.mkdir(mode=0o700)
    (creds / "t.key").write_text("sk-offline-dummy-not-a-key\n")
    config = scratch / "do.toml"
    config.write_text('[remote]\napi_base = "https://localhost:9"\napi_style = "open-webui"\n'
                      f'models = ["{MODEL}"]\ncredential = "t.key"\n')
    env = {k: v for k, v in os.environ.items() if k not in ("SSL_CERT_FILE", "SSL_CERT_DIR")}
    env.update(AGENTIC_RUNTIME_DIR=str(runtime), CREDENTIALS_DIRECTORY=str(creds))
    log = scratch / "providerd.log"
    proc = subprocess.Popen([str(bin_dir / "providerd"), "--remote-config", str(config), "--discover-only"],
                            env=env, stdout=log.open("wb"), stderr=subprocess.STDOUT)
    try:
        sock = runtime / "providerd.sock"
        for _ in range(200):
            if proc.poll() is not None:
                raise AssertionError(log.read_text())
            if sock.exists():
                break
            time.sleep(0.05)
        reply = acceptance.request(str(sock), "provider.submit", {
            "request_id": "r1", "prompt": acceptance.PROMPT, "created_by": "uid:1"})
        assert reply["type"] == "err" and reply["code"] == "permission_denied", reply
        assert "discover_only; outcome not_sent" in reply["message"], reply
        result = client("discover", str(sock))
        report = json.loads(result.stdout)
        assert report["prompt_sent"] is False and "address_refused" in report["error"]["message"], report
    finally:
        proc.send_signal(signal.SIGTERM)
        proc.wait(timeout=10)
    text = log.read_text()
    assert "restricted to discovery" in text and acceptance.PROMPT not in text, text


def check_dns_classification():
    for public in ["188.114.96.1", "188.114.97.1", "2a06:98c1:3120::1", "2a06:98c1:3121::1", "1.1.1.1"]:
        assert acceptance.classify(public) == "public", public
    for refused in ["10.0.0.1", "172.16.0.1", "192.168.0.1", "100.64.0.1", "127.0.0.1",
                    "169.254.169.254", "0.0.0.0", "224.0.0.1", "192.0.2.1", "::1",
                    "fd00::1", "fe80::1", "::ffff:10.0.0.1", "64:ff9b::a00:1", "64:ff9b::808:808",
                    "::a00:1", "::808:808", "ff02::1", "2001:db8::1", "198.18.0.1", "240.0.0.1"]:
        assert acceptance.classify(refused) == "refused", refused


def check_evidence(scratch):
    journal = scratch / "journal"
    reports = scratch / "reports"
    reports.write_text(json.dumps({"step": "discover", "prompt_sent": False, "ok": True}) + "\n")
    base = dict(os.environ, STEP="preflight", INSTANCE=INSTANCE, APPROVAL="", JOURNAL=str(journal),
                REPORTS=str(reports), GIT_COMMIT="0" * 40, GIT_CLEAN="yes", SYSTEMD_VERSION="systemd 255",
                EXPOSURE="", SHA_PROVIDERD="a", SHA_CLIENT="b", SHA_UNIT="c", SHA_DROPIN="d", SHA_CONFIG="e")
    for name, line in [("bearer", "authorization: Bearer abc"), ("token", "key sk-live-123"),
                       ("prompt", acceptance.PROMPT)]:
        journal.write_text(f"providerd started\n{line}\n")
        target = scratch / f"evidence-{name}.json"
        result = run([sys.executable, "-I", str(CLIENT), "evidence"],
                     env=dict(base, EVIDENCE_PATH=str(target)), check=False)
        assert result.returncode == 1 and not target.exists(), (name, result.stdout)
    journal.write_text("providerd started\nremote backend restricted to discovery\n")
    target = scratch / "evidence-clean.json"
    result = run([sys.executable, "-I", str(CLIENT), "evidence"], env=dict(base, EVIDENCE_PATH=str(target)))
    record = json.loads(target.read_text())
    assert record["journal_scan"] == {"contains_prompt": False, "contains_bearer": False,
                                      "contains_sk_token": False, "lines": 2}, record
    assert record["steps"][0]["prompt_sent"] is False


def check_privileged_refusals():
    if os.geteuid() == 0:
        print("running as root: privileged-refusal check skipped")
        return
    for args in (["install", INSTANCE, API_BASE, STYLE, MODEL], ["preflight", INSTANCE],
                 ["accept", INSTANCE, "approval"], ["disable", INSTANCE], ["rollback", INSTANCE]):
        result = run(["bash", str(OPERATOR), *args], check=False)
        assert result.returncode != 0 and "run as root" in result.stderr, (args, result.stderr)


def main(bin_dir):
    check_unit_template()
    check_dns_classification()
    check_privileged_refusals()
    # Short paths: Unix socket names are limited to 108 bytes.
    with tempfile.TemporaryDirectory(prefix="fl-op-", dir="/tmp") as scratch:
        scratch = pathlib.Path(scratch)
        dest = check_render(scratch, bin_dir)
        check_systemd(dest, bin_dir)
        check_client(scratch)
        check_discover_only(scratch, bin_dir)
        check_evidence(scratch)
    print("remote provider operator contract: unit confinement, render, discover-only, "
          "single-prompt client, redacted evidence, DNS classification, root-only subcommands")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

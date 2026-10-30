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
- every privileged subcommand refuses to run without root;
- install and rollback (against scratch system roots) touch only what the
  ledger says this tool created: rollback never removes another
  instance's files or runtime resources, a foreign or modified file, or
  the shared binary, account and groups while anything else uses them;
- `--require-isolated-identity` refuses supplementary groups and root;
- the unit probe detects an unconfined process and does not mistake an
  unanswered connection for a blocked one.
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
        "--require-isolated-identity\n",
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
    assert "--require-isolated-identity\n" in dropin, dropin
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
    assert record["approval_reference"] is None
    assert "not cryptographic proof" in record["approval_reference_semantics"], record
    assert set(record["identity"]) == {"client_group_members", "provider_supplementary_groups"}, record
    assert record["journal_scan"] == {"contains_prompt": False, "contains_bearer": False,
                                      "contains_sk_token": False, "lines": 2}, record
    assert record["steps"][0]["prompt_sent"] is False



# --- install and rollback against scratch system roots -------------------

BASE_PASSWD = "root:x:0:0::/root:/bin/bash\nflowerly:x:961:961::/var/lib/flowerly:/usr/sbin/nologin\n"
BASE_GROUP = "root:x:0:\nflowerly:x:961:\n"


class Sysroot:
    """A scratch system root; the operator script logs instead of running
    system commands and reads accounts, processes and units from files."""

    def __init__(self, scratch, name, build, instances=("alpha", "beta")):
        self.root = scratch / name
        (self.root / "test").mkdir(parents=True)
        (self.root / "etc/credstore.encrypted").mkdir(parents=True)
        (self.root / "etc/passwd").write_text(BASE_PASSWD)
        (self.root / "etc/group").write_text(BASE_GROUP)
        for instance in instances:
            (self.root / f"etc/credstore.encrypted/{instance}.key").write_bytes(b"opaque-encrypted-blob")
        self.build = build
        self.resolv = scratch / "resolv-lan"

    def op(self, *args, build=None):
        env = dict(os.environ, FLOWERLY_OPERATOR_SYSROOT=str(self.root), FLOWERLY_OPERATOR_TEST="1",
                   FLOWERLY_OPERATOR_PROVIDERD=str(build or self.build), RESOLV_CONF=str(self.resolv))
        return run(["bash", str(OPERATOR), *args], env=env, check=False)

    def install(self, instance, **kw):
        result = self.op("install", instance, API_BASE, STYLE, MODEL, **kw)
        assert result.returncode == 0, result.stdout + result.stderr
        return result

    def files(self):
        return {str(p.relative_to(self.root)): p.read_bytes() for p in self.root.rglob("*")
                if (p.is_file() or p.is_symlink()) and not str(p.relative_to(self.root)).startswith("test/")}

    def commands(self):
        log = self.root / "test/commands.log"
        return log.read_text().splitlines() if log.exists() else []

    def write(self, rel, text):
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


SHARED = ["usr/local/libexec/flowerly/providerd", "usr/local/libexec/flowerly/remote-provider-acceptance.py",
          "etc/systemd/system/flowerly-provider-remote@.service", "etc/sysusers.d/flowerly-provider.conf"]


def instance_paths(instance):
    return [f"etc/systemd/system/flowerly-provider-remote@{instance}.service.d/10-operator.conf",
            f"etc/flowerly/providers/{instance}.service.toml", f"etc/flowerly/providers/{instance}.toml"]


def deletions(sysroot):
    return [c for c in sysroot.commands() if c.startswith(("userdel", "groupdel"))]


def check_rollback(scratch):
    (scratch / "resolv-lan").write_text("nameserver 192.168.0.1\n")
    build = scratch / "providerd-build"
    build.write_text("#!/bin/sh\n# fake providerd build A\n")
    build.chmod(0o755)
    other_build = scratch / "providerd-build-b"
    other_build.write_text("#!/bin/sh\n# fake providerd build B\n")
    other_build.chmod(0o755)

    # 1. One instance: rollback removes exactly what install created.
    sr = Sysroot(scratch, "single", build)
    baseline = sr.files()
    sr.install("alpha")
    files = sr.files()
    for rel in SHARED + instance_paths("alpha"):
        assert rel in files, rel
    assert "flowerly-provider:" in (sr.root / "etc/passwd").read_text()
    sr.write("run/flowerly-provider/alpha.sock", "")
    sr.write("run/flowerly-provider/alpha.sock.lock", "")
    result = sr.op("rollback", "alpha")
    assert result.returncode == 0, result.stderr
    assert sr.files() == baseline, set(sr.files()) ^ set(baseline)
    assert deletions(sr) == ["userdel flowerly-provider", "groupdel flowerly-provider-clients"] or \
        deletions(sr) == ["userdel flowerly-provider", "groupdel flowerly-provider", "groupdel flowerly-provider-clients"], deletions(sr)
    assert not (sr.root / "run/flowerly-provider").exists()

    # 2. Two instances: rolling back one leaves the other, its runtime
    #    files and everything shared untouched.
    sr = Sysroot(scratch, "two", build)
    sr.install("alpha")
    sr.install("beta")
    sr.write("run/flowerly-provider/alpha.sock", "")
    sr.write("run/flowerly-provider/beta.sock", "")
    sr.write("run/flowerly-provider/beta.sock.lock", "")
    before = sr.files()
    result = sr.op("rollback", "alpha")
    assert result.returncode == 0, result.stderr
    after = sr.files()
    for rel in SHARED + instance_paths("beta") + ["run/flowerly-provider/beta.sock", "run/flowerly-provider/beta.sock.lock"]:
        assert after.get(rel) == before[rel], f"rollback of alpha changed {rel}"
    for rel in instance_paths("alpha") + ["run/flowerly-provider/alpha.sock"]:
        assert rel not in after, rel
    assert deletions(sr) == [], deletions(sr)
    assert "instance beta (this tool)" in result.stdout, result.stdout
    assert sr.op("rollback", "beta").returncode == 0
    assert "userdel flowerly-provider" in deletions(sr)

    # 3. Shared resources that existed before install (another
    #    installation) are reused and never removed.
    sr = Sysroot(scratch, "foreign", build)
    sr.write("usr/local/libexec/flowerly/providerd", build.read_text())
    (sr.root / "etc/passwd").write_text(BASE_PASSWD + "flowerly-provider:x:990:990::/:/usr/sbin/nologin\n")
    (sr.root / "etc/group").write_text(BASE_GROUP + "flowerly-provider:x:990:\nflowerly-provider-clients:x:989:\n")
    baseline = sr.files()
    sr.install("alpha")
    assert sr.op("rollback", "alpha").returncode == 0
    assert sr.files() == baseline, set(sr.files()) ^ set(baseline)
    assert deletions(sr) == [], deletions(sr)

    # 4. A foreign file that differs is never replaced: install refuses
    #    before changing anything.
    sr = Sysroot(scratch, "conflict", build)
    sr.write("usr/local/libexec/flowerly/providerd", "#!/bin/sh\n# somebody else's providerd\n")
    baseline = sr.files()
    result = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
    assert result.returncode != 0 and "not installed by this tool" in result.stderr, result.stderr
    assert sr.files() == baseline and sr.commands() == [], sr.commands()

    # 5. A different build while an instance is installed is refused; the
    #    shared binary serves the other instance.
    sr = Sysroot(scratch, "upgrade", build)
    sr.install("alpha")
    before = sr.files()
    result = sr.op("install", "beta", API_BASE, STYLE, MODEL, build=other_build)
    assert result.returncode != 0 and "differs from this build" in result.stderr, result.stderr
    assert sr.files() == before

    # 6. A provider config of another installation keeps everything shared.
    sr = Sysroot(scratch, "foreign-config", build)
    sr.install("alpha")
    sr.write("etc/flowerly/providers/other.toml", "[remote]\n")
    result = sr.op("rollback", "alpha")
    assert result.returncode == 0 and "provider config other (another installation)" in result.stdout, result.stdout
    assert deletions(sr) == [] and all(rel in sr.files() for rel in SHARED)

    # 7. An active unit of another instance keeps everything shared.
    sr = Sysroot(scratch, "active", build)
    sr.install("alpha")
    sr.write("test/active-units", "flowerly-provider-remote@other.service\n")
    result = sr.op("rollback", "alpha")
    assert "active unit flowerly-provider-remote@other.service" in result.stdout, result.stdout
    assert deletions(sr) == [] and all(rel in sr.files() for rel in SHARED)

    # 8. A process still running as the provider account keeps the account
    #    and its groups; rollback-shared finishes once it has gone.
    sr = Sysroot(scratch, "process", build)
    sr.install("alpha")
    sr.write("test/processes", "flowerly-provider\n")
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "still runs processes" in result.stderr, result.stderr
    assert deletions(sr) == [], deletions(sr)
    assert "flowerly-provider:" in (sr.root / "etc/passwd").read_text()
    assert not any(rel in sr.files() for rel in SHARED), "unused shared files should go"
    (sr.root / "test/processes").write_text("")
    assert sr.op("rollback-shared").returncode == 0
    assert "userdel flowerly-provider" in deletions(sr)
    assert not (sr.root / "var/lib/flowerly-provider-operator").exists()

    # 9. flowerlyd joined the socket group (gates enabled): the group stays.
    sr = Sysroot(scratch, "gate", build)
    sr.install("alpha")
    group = (sr.root / "etc/group").read_text().replace("flowerly-provider-clients:x:989:", "flowerly-provider-clients:x:989:flowerly")
    (sr.root / "etc/group").write_text(group)
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "kept group flowerly-provider-clients: still in use by flowerly" in result.stderr, result.stderr
    assert "groupdel flowerly-provider-clients" not in deletions(sr)

    # 10. An operator-edited file is kept; the rest is removed.
    sr = Sysroot(scratch, "edited", build)
    sr.install("alpha")
    sr.write("etc/flowerly/providers/alpha.toml", "# edited by the operator\n")
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "kept /etc/flowerly/providers/alpha.toml: modified" in result.stderr, result.stderr
    assert "etc/flowerly/providers/alpha.toml" in sr.files()
    assert "etc/flowerly/providers/alpha.service.toml" not in sr.files()

    # 11. A tampered ledger removes nothing at all.
    for name, ledger, line in [
        ("tamper-instance", "instances/alpha", "file\tetc/passwd\t" + "0" * 64),
        ("tamper-shared", "shared", "file\tetc/shadow\t" + "0" * 64),
        ("tamper-user", "shared", "user\troot\t-"),
    ]:
        sr = Sysroot(scratch, name, build)
        sr.install("alpha")
        path = sr.root / "var/lib/flowerly-provider-operator" / ledger
        path.write_text(path.read_text() + line + "\n")
        before, commands = sr.files(), sr.commands()
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and "unexpected entry" in result.stderr, (name, result.stderr)
        assert sr.files() == before and sr.commands() == commands, name

    # 12. An instance this tool did not install: nothing is removed.
    sr = Sysroot(scratch, "unknown", build)
    sr.install("alpha")
    sr.write("etc/flowerly/providers/beta.toml", "[remote]\n")
    before, commands = sr.files(), sr.commands()
    result = sr.op("rollback", "beta")
    assert result.returncode != 0 and "did not install it" in result.stderr, result.stderr
    assert sr.files() == before and sr.commands() == commands

    # 13. A symlink planted at the instance's socket path is not followed.
    sr = Sysroot(scratch, "symlink", build)
    sr.install("alpha")
    victim = sr.root / "etc/important"
    victim.write_text("keep me\n")
    (sr.root / "run/flowerly-provider").mkdir(parents=True)
    (sr.root / "run/flowerly-provider/alpha.sock").symlink_to(victim)
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "a symlink" in result.stderr, result.stderr
    assert victim.read_text() == "keep me\n"

    # 14. Identity: an intruder in the socket group, or the provider
    #     account in another group, makes install refuse with no change.
    for name, group_text in [
        ("intruder", BASE_GROUP + "flowerly-provider-clients:x:989:alice\n"),
        ("provider-in-flowerly", "root:x:0:\nflowerly:x:961:flowerly-provider\n"),
    ]:
        sr = Sysroot(scratch, name, build)
        if name == "provider-in-flowerly":
            (sr.root / "etc/passwd").write_text(BASE_PASSWD + "flowerly-provider:x:990:990::/:/usr/sbin/nologin\n")
        (sr.root / "etc/group").write_text(group_text)
        baseline = sr.files()
        result = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
        assert result.returncode != 0 and ("only flowerly may join" in result.stderr
                                           or "must hold no other group" in result.stderr), (name, result.stderr)
        assert sr.files() == baseline and sr.commands() == [], name

    # 15. The test hooks cannot be reached without the explicit test flag.
    env = dict(os.environ, FLOWERLY_OPERATOR_SYSROOT=str(scratch))
    result = run(["bash", str(OPERATOR), "rollback", "alpha"], env=env, check=False)
    assert result.returncode != 0 and "offline tests only" in result.stderr, result.stderr


def check_isolated_identity(scratch, bin_dir):
    """`--require-isolated-identity` against this process's real identity."""
    status = pathlib.Path("/proc/self/status").read_text()
    field = lambda key: next(l.split()[1:] for l in status.splitlines() if l.startswith(key))
    uids, gids, groups = field("Uid:"), field("Gid:"), field("Groups:")
    isolated = "0" not in uids + gids and all(g == gids[1] for g in groups)
    creds = scratch / "creds-id"
    creds.mkdir(mode=0o700)
    (creds / "t.key").write_text("sk-offline-dummy-not-a-key\n")
    config = scratch / "id.toml"
    config.write_text(f'[remote]\napi_base = "https://localhost:9"\napi_style = "openai"\nmodels = ["{MODEL}"]\ncredential = "t.key"\n')
    env = {k: v for k, v in os.environ.items() if k not in ("SSL_CERT_FILE", "SSL_CERT_DIR")}
    env["CREDENTIALS_DIRECTORY"] = str(creds)
    result = run([str(bin_dir / "providerd"), "--remote-config", str(config), "--require-isolated-identity",
                  "--self-test"], env=env, check=False)
    if isolated:
        assert result.returncode == 0, result.stderr
    else:
        assert result.returncode != 0, "a process with supplementary groups was accepted"
        assert "supplementary group" in result.stderr or "uid or gid 0" in result.stderr, result.stderr
    print(f"isolated-identity check: {'accepted' if isolated else 'refused'} this process "
          f"(groups {' '.join(groups) or 'none'})")


PROBE = ROOT / "scripts/provider-unit-probe.py"


def check_probe(scratch):
    """The KVM plan's probe, run here without any sandbox, must find the
    boundary open; its classification is checked on fixed inputs."""
    spec = importlib.util.spec_from_file_location("probe", PROBE)
    probe = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(probe)

    results = {}
    def refused():
        raise ConnectionRefusedError(111, "refused")
    def blocked():
        raise PermissionError(1, "blocked")
    def unreachable():
        raise TimeoutError(110, "timed out")
    probe.must_fail("refused", refused, results, probe.BLOCKED)
    probe.must_fail("blocked", blocked, results, probe.BLOCKED)
    probe.must_fail("timeout", unreachable, results, probe.BLOCKED)
    probe.must_fail("allowed", lambda: None, results)
    probe.must_fail("missing", lambda: open(scratch / "absent"), results)
    assert [results[k]["ok"] for k in ("refused", "blocked", "timeout", "allowed", "missing")] == \
        [False, True, False, False, True], results

    creds = scratch / "probe-creds"
    creds.mkdir()
    (creds / "probe.key").write_text("")
    for extra, expect_ok in [(None, True), ("other.key", False)]:
        if extra:
            (creds / extra).write_text("")
        env = dict(os.environ, CREDENTIALS_DIRECTORY=str(creds))
        result = run([sys.executable, "-I", str(PROBE)], env=env, check=False)
        report = json.loads(result.stdout)
        check = report["checks"]["credentials directory holds only this instance's key"]
        assert check["ok"] is expect_ok, check
        # Unconfined, the probe must always report a violation and fail.
        assert result.returncode == 1 and report["violations"], report["violations"]
    env = {k: v for k, v in os.environ.items() if k != "CREDENTIALS_DIRECTORY"}
    report = json.loads(run([sys.executable, "-I", str(PROBE)], env=env, check=False).stdout)
    assert "credentials directory holds only this instance's key" in report["violations"]
    assert len(report["checks"]) >= 40, len(report["checks"])

def check_privileged_refusals():
    if os.geteuid() == 0:
        print("running as root: privileged-refusal check skipped")
        return
    for args in (["install", INSTANCE, API_BASE, STYLE, MODEL], ["preflight", INSTANCE],
                 ["accept", INSTANCE, "approval"], ["disable", INSTANCE], ["rollback", INSTANCE],
                 ["rollback-shared"]):
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
        check_rollback(scratch)
        check_isolated_identity(scratch, bin_dir)
        check_probe(scratch)
    print("remote provider operator contract: unit confinement, render, discover-only, "
          "single-prompt client, redacted evidence, DNS classification, root-only subcommands, "
          "ledger-scoped rollback (15 negative cases), isolated identity, unit probe detection")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

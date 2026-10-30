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
  any shared resource while a usage check finds a user or cannot
  complete; a file it keeps stays in the ledger and a rerun finishes the
  job; one exclusive lock serializes every mutating subcommand; an
  interruption at any change is recovered by rerunning, and never
  removes an identical file or account another installation created
  meanwhile; symlinked or
  writable parent directories are refused before any change;
- `--require-isolated-identity` refuses supplementary groups and root;
- the unit probe detects an unconfined process and does not mistake an
  unanswered connection for a blocked one.
"""
import importlib.util
import json
import re
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
LOCK = "run/flowerly-provider-operator.lock"
LEDGER = "var/lib/flowerly-provider-operator"
LOCKED = "holds /run/flowerly-provider-operator.lock"
AMBIGUOUS = re.compile(r"kept (account|group) (\S+): an interrupted install may not have created it")


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

    def env(self, build=None, **extra):
        return dict(os.environ, FLOWERLY_OPERATOR_SYSROOT=str(self.root), FLOWERLY_OPERATOR_TEST="1",
                    FLOWERLY_OPERATOR_PROVIDERD=str(build or self.build), RESOLV_CONF=str(self.resolv), **extra)

    def op(self, *args, build=None, **extra):
        return run(["bash", str(OPERATOR), *args], env=self.env(build, **extra), check=False)

    def install(self, instance, **kw):
        result = self.op("install", instance, API_BASE, STYLE, MODEL, **kw)
        assert result.returncode == 0, result.stdout + result.stderr
        return result

    def files(self):
        """Every file and symlink outside the test inputs and the lock file
        (which, by design, outlives each command)."""
        return {str(p.relative_to(self.root)): p.read_bytes() if not p.is_symlink() else b"-> " + bytes(p.readlink())
                for p in self.root.rglob("*")
                if (p.is_file() or p.is_symlink()) and not str(p.relative_to(self.root)).startswith("test/")
                and str(p.relative_to(self.root)) != LOCK}

    def commands(self):
        log = self.root / "test/commands.log"
        return log.read_text().splitlines() if log.exists() else []

    def write(self, rel, text):
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def ledger(self, name):
        path = self.root / LEDGER / name
        return path.read_text().splitlines() if path.exists() else None

    def recover(self, instance="alpha", strict=True):
        """The documented recovery after an interruption: rerun `rollback`
        while the instance ledger exists, then `rollback-shared`. Returns
        the accounts and groups it reported as possibly not its own."""
        output = ""
        for args, ledger in [(("rollback", instance), f"instances/{instance}"), (("rollback-shared",), "shared")]:
            if self.ledger(ledger) is None:
                continue
            result = self.op(*args)
            output += result.stderr
            ambiguous = AMBIGUOUS.findall(result.stderr)
            if strict:
                assert result.returncode == 0 or ambiguous, result.stdout + result.stderr
        return sorted(set(AMBIGUOUS.findall(output)))

    def remove_account_lines(self, reported):
        """What the operator does by hand for an account or group reported
        as possibly not created by this tool, once they checked it is unused."""
        for kind, rel in (("account", "etc/passwd"), ("group", "etc/group")):
            names = {name for k, name in reported if k == kind}
            lines = (self.root / rel).read_text().splitlines(keepends=True)
            (self.root / rel).write_text("".join(l for l in lines if l.split(":")[0] not in names))


SHARED = ["usr/local/libexec/flowerly/providerd", "usr/local/libexec/flowerly/remote-provider-acceptance.py",
          "etc/systemd/system/flowerly-provider-remote@.service", "etc/sysusers.d/flowerly-provider.conf"]


def instance_paths(instance):
    return [f"etc/systemd/system/flowerly-provider-remote@{instance}.service.d/10-operator.conf",
            f"etc/flowerly/providers/{instance}.service.toml", f"etc/flowerly/providers/{instance}.toml"]


def deletions(sysroot):
    return [c for c in sysroot.commands() if c.startswith(("userdel", "groupdel"))]


def builds(scratch):
    (scratch / "resolv-lan").write_text("nameserver 192.168.0.1\n")
    build = scratch / "providerd-build"
    if not build.exists():
        build.write_text("#!/bin/sh\n# fake providerd build A\n")
        build.chmod(0o755)
    other_build = scratch / "providerd-build-b"
    if not other_build.exists():
        other_build.write_text("#!/bin/sh\n# fake providerd build B\n")
        other_build.chmod(0o755)
    return build, other_build


def case_single_instance(scratch, build, other_build):
    """One instance: rollback removes exactly what install created, and a
    rerun is a refusal that changes nothing."""
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
    commands = sr.commands()
    for args, refusal in [(("rollback", "alpha"), "no install ledger"), (("rollback-shared",), "no shared resources")]:
        result = sr.op(*args)
        assert result.returncode != 0 and refusal in result.stderr, result.stderr
        assert sr.files() == baseline and sr.commands() == commands


def case_two_instances(scratch, build, other_build):
    """Rolling back one of two instances leaves the other, its runtime files
    and everything shared untouched."""
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


def case_foreign_shared(scratch, build, other_build):
    """Shared resources that existed before install are reused, never removed."""
    sr = Sysroot(scratch, "foreign", build)
    sr.write("usr/local/libexec/flowerly/providerd", build.read_text())
    (sr.root / "etc/passwd").write_text(BASE_PASSWD + "flowerly-provider:x:990:990::/:/usr/sbin/nologin\n")
    (sr.root / "etc/group").write_text(BASE_GROUP + "flowerly-provider:x:990:\nflowerly-provider-clients:x:989:\n")
    baseline = sr.files()
    sr.install("alpha")
    assert sr.op("rollback", "alpha").returncode == 0
    assert sr.files() == baseline, set(sr.files()) ^ set(baseline)
    assert deletions(sr) == [], deletions(sr)


def case_foreign_conflict(scratch, build, other_build):
    """A foreign file that differs is never replaced: install refuses first."""
    sr = Sysroot(scratch, "conflict", build)
    sr.write("usr/local/libexec/flowerly/providerd", "#!/bin/sh\n# somebody else's providerd\n")
    baseline = sr.files()
    result = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
    assert result.returncode != 0 and "not installed by this tool" in result.stderr, result.stderr
    assert sr.files() == baseline and sr.commands() == [], sr.commands()


def case_other_build(scratch, build, other_build):
    """A different build while an instance is installed is refused."""
    sr = Sysroot(scratch, "upgrade", build)
    sr.install("alpha")
    before = sr.files()
    result = sr.op("install", "beta", API_BASE, STYLE, MODEL, build=other_build)
    assert result.returncode != 0 and "differs from this build" in result.stderr, result.stderr
    assert sr.files() == before


def case_other_installation(scratch, build, other_build):
    """A foreign provider config or another active unit keeps everything shared."""
    for name, rel, text, expect in [
        ("foreign-config", "etc/flowerly/providers/other.toml", "[remote]\n", "provider config other (another installation)"),
        ("active", "test/active-units", "flowerly-provider-remote@other.service\n", "active unit flowerly-provider-remote@other.service"),
    ]:
        sr = Sysroot(scratch, name, build)
        sr.install("alpha")
        sr.write(rel, text)
        shared = {rel: sr.files()[rel] for rel in SHARED}
        result = sr.op("rollback", "alpha")
        assert result.returncode == 0 and expect in result.stdout, (name, result.stdout)
        assert deletions(sr) == [] and {rel: sr.files().get(rel) for rel in SHARED} == shared, name


def case_consumer_blocks_all_shared(scratch, build, other_build):
    """BLOCKER 1: every usage check runs before any shared change. A process
    of the provider account, a process executing the shared binary, an
    outside member of an owned group, or a check that cannot complete
    leaves every shared file, account, group and the shared ledger as it
    was; rollback-shared finishes once the condition is gone."""
    for name, rel, text, expect in [
        ("process", "test/processes", "flowerly-provider\n", "processes running as flowerly-provider"),
        ("exe", "test/exe", "4242 /usr/local/libexec/flowerly/providerd\n",
         "processes executing /usr/local/libexec/flowerly/providerd: 4242"),
        ("exe-deleted", "test/exe", "4243 /usr/local/libexec/flowerly/providerd (deleted)\n", "providerd: 4243"),
        ("gate-member", None, None, "member flowerly of group flowerly-provider-clients"),
        ("processes-unknown", "test/processes-error", "", "unknown: processes of flowerly-provider"),
        ("units-unknown", "test/units-error", "", "unknown: active provider units"),
        ("exe-unknown", "test/exe-error", "", "unknown: processes executing"),
    ]:
        sr = Sysroot(scratch, f"consumer-{name}", build)
        baseline = sr.files()
        sr.install("alpha")
        if rel:
            sr.write(rel, text)
        else:
            group = (sr.root / "etc/group").read_text()
            (sr.root / "etc/group").write_text(group.replace("flowerly-provider-clients:x:989:", "flowerly-provider-clients:x:989:flowerly"))
        accounts = ((sr.root / "etc/passwd").read_text(), (sr.root / "etc/group").read_text())
        shared = {rel: sr.files()[rel] for rel in SHARED}
        shared_ledger = sr.ledger("shared")
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and expect in result.stdout, (name, result.stdout, result.stderr)
        assert {rel: sr.files().get(rel) for rel in SHARED} == shared, f"{name}: a shared file changed"
        assert sr.ledger("shared") == shared_ledger, f"{name}: the shared ledger changed"
        assert deletions(sr) == [], (name, deletions(sr))
        assert ((sr.root / "etc/passwd").read_text(), (sr.root / "etc/group").read_text()) == accounts, name
        for rel_ in instance_paths("alpha"):
            assert rel_ not in sr.files(), (name, rel_)
        # Still in use: rollback-shared refuses the same way.
        again = sr.op("rollback-shared")
        assert again.returncode != 0 and expect in again.stdout, (name, again.stdout)
        assert {rel: sr.files().get(rel) for rel in SHARED} == shared and deletions(sr) == [], name
        # The condition is gone: rollback-shared completes.
        for path in ("test/processes", "test/exe", "test/processes-error", "test/units-error", "test/exe-error"):
            (sr.root / path).unlink(missing_ok=True)
        (sr.root / "etc/group").write_text((sr.root / "etc/group").read_text().replace(
            "flowerly-provider-clients:x:989:flowerly", "flowerly-provider-clients:x:989:"))
        result = sr.op("rollback-shared")
        assert result.returncode == 0, (name, result.stdout, result.stderr)
        assert sr.files() == baseline, (name, set(sr.files()) ^ set(baseline))
        assert "userdel flowerly-provider" in deletions(sr), name


def case_modified_file_keeps_ledger(scratch, build, other_build):
    """BLOCKER 2: a modified file is kept and stays in the ledger; the
    rerun is idempotent until the operator restores or removes the file,
    then it completes. A partly rolled back instance is never started."""
    for name, resolve in [("edited-restore", "restore"), ("edited-remove", "remove")]:
        sr = Sysroot(scratch, name, build)
        baseline = sr.files()
        sr.install("alpha")
        original = (sr.root / "etc/flowerly/providers/alpha.toml").read_text()
        full_ledger = sr.ledger("instances/alpha")
        sr.write("etc/flowerly/providers/alpha.toml", "# edited by the operator\n")
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and "kept /etc/flowerly/providers/alpha.toml: modified" in result.stderr, result.stderr
        assert "etc/flowerly/providers/alpha.toml" in sr.files()
        assert "etc/flowerly/providers/alpha.service.toml" not in sr.files()
        kept = sr.ledger("instances/alpha")
        assert kept == [l for l in full_ledger if "\tetc/flowerly/providers/alpha.toml\t" in l] and len(kept) == 1, kept
        assert all(rel in sr.files() for rel in SHARED) and deletions(sr) == [], "shared resources must stay"
        # Rerun with the file still modified: same refusal, same state.
        state, commands = sr.files(), [c for c in sr.commands() if not c.startswith("systemctl")]
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and "modified since install" in result.stderr, result.stderr
        assert sr.files() == state and sr.ledger("instances/alpha") == kept
        assert [c for c in sr.commands() if not c.startswith("systemctl")] == commands
        # A partly rolled back instance cannot be preflighted or accepted.
        for args in (("preflight", "alpha"), ("accept", "alpha", "attestation")):
            refused = sr.op(*args)
            assert refused.returncode != 0 and "not completely installed" in refused.stderr, refused.stderr
        # Re-installing over the remains is refused too.
        refused = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
        assert refused.returncode != 0 and "already installed" in refused.stderr, refused.stderr
        if resolve == "restore":
            sr.write("etc/flowerly/providers/alpha.toml", original)
        else:
            (sr.root / "etc/flowerly/providers/alpha.toml").unlink()
        result = sr.op("rollback", "alpha")
        assert result.returncode == 0, result.stdout + result.stderr
        assert sr.files() == baseline, (name, set(sr.files()) ^ set(baseline))
        assert "userdel flowerly-provider" in deletions(sr)


def case_installed_instance_preflight(scratch, build, other_build):
    """An intact install passes the ledger check and reaches the test-mode
    refusal; an edited file stops preflight before anything runs."""
    sr = Sysroot(scratch, "preflight-check", build)
    sr.install("alpha")
    result = sr.op("preflight", "alpha")
    assert result.returncode != 0 and "not available in test mode" in result.stderr, result.stderr
    sr.write("etc/flowerly/providers/alpha.toml", "# edited\n")
    result = sr.op("preflight", "alpha")
    assert result.returncode != 0 and "modified since install" in result.stderr, result.stderr


def case_tampered_ledger(scratch, build, other_build):
    """A tampered ledger removes nothing at all."""
    for name, ledger, line in [
        ("tamper-instance", "instances/alpha", "file\tetc/passwd\t" + "0" * 64),
        ("tamper-shared", "shared", "file\tetc/shadow\t" + "0" * 64),
        ("tamper-user", "shared", "user\troot\t-"),
    ]:
        sr = Sysroot(scratch, name, build)
        sr.install("alpha")
        path = sr.root / LEDGER / ledger
        path.write_text(path.read_text() + line + "\n")
        before, commands = sr.files(), sr.commands()
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and "unexpected entry" in result.stderr, (name, result.stderr)
        assert sr.files() == before and sr.commands() == commands, name


def case_unknown_instance(scratch, build, other_build):
    """An instance this tool did not install: nothing is removed."""
    sr = Sysroot(scratch, "unknown", build)
    sr.install("alpha")
    sr.write("etc/flowerly/providers/beta.toml", "[remote]\n")
    before, commands = sr.files(), sr.commands()
    result = sr.op("rollback", "beta")
    assert result.returncode != 0 and "did not install it" in result.stderr, result.stderr
    assert sr.files() == before and sr.commands() == commands


def case_symlinks(scratch, build, other_build):
    """MAJOR: symlink replacement of a file, of the runtime socket, of each
    managed parent directory and of the lock is never followed. Each
    decoy holds byte-identical copies, so a check by content alone would
    pass and delete them."""
    # A file replaced by a symlink to an identical copy: kept, ledger kept.
    sr = Sysroot(scratch, "symlink-file", build)
    sr.install("alpha")
    target = sr.root / "etc/flowerly/providers/alpha.toml"
    decoy = sr.root / "etc/decoy.toml"
    decoy.write_bytes(target.read_bytes())
    target.unlink()
    target.symlink_to(decoy)
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "no longer the regular file" in result.stderr, result.stderr
    assert decoy.exists() and target.is_symlink()
    assert len(sr.ledger("instances/alpha")) == 1

    # The socket path.
    sr = Sysroot(scratch, "symlink-socket", build)
    sr.install("alpha")
    victim = sr.root / "etc/important"
    victim.write_text("keep me\n")
    (sr.root / "run/flowerly-provider").mkdir(parents=True)
    (sr.root / "run/flowerly-provider/alpha.sock").symlink_to(victim)
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "a symlink" in result.stderr, result.stderr
    assert victim.read_text() == "keep me\n"

    # A managed parent directory replaced by a symlink to identical copies:
    # refused before any change, the decoy untouched.
    for name, directory in [("symlink-providers", "etc/flowerly/providers"),
                            ("symlink-dropin", "etc/systemd/system/flowerly-provider-remote@alpha.service.d"),
                            ("symlink-libexec", "usr/local/libexec/flowerly"),
                            ("symlink-ledger", f"{LEDGER}/instances"),
                            ("symlink-runtime", "run/flowerly-provider")]:
        sr = Sysroot(scratch, name, build)
        sr.install("alpha")
        real = sr.root / directory
        real.mkdir(parents=True, exist_ok=True)
        decoy = sr.root / f"decoy-{name}"
        shutil.copytree(real, decoy)
        shutil.rmtree(real)
        real.symlink_to(decoy)
        before, commands = sr.files(), sr.commands()
        decoy_files = {p.name: p.read_bytes() for p in decoy.iterdir()}
        result = sr.op("rollback", "alpha")
        assert result.returncode != 0 and ("not a real directory" in result.stderr or "is a symlink" in result.stderr), \
            (name, result.stderr)
        assert {p.name: p.read_bytes() for p in decoy.iterdir()} == decoy_files, f"{name}: the decoy was changed"
        assert sr.files() == before and sr.commands() == commands, f"{name}: something changed"

    # A directory others can write: refused before any change.
    sr = Sysroot(scratch, "writable-parent", build)
    sr.install("alpha")
    (sr.root / "etc/flowerly/providers").chmod(0o777)
    before, commands = sr.files(), sr.commands()
    result = sr.op("rollback", "alpha")
    assert result.returncode != 0 and "closed to group and others" in result.stderr, result.stderr
    assert sr.files() == before and sr.commands() == commands
    (sr.root / "etc/flowerly/providers").chmod(0o755)

    # Install through a symlinked parent: refused, nothing written anywhere.
    sr = Sysroot(scratch, "symlink-install", build)
    decoy = sr.root / "decoy-etc-flowerly"
    decoy.mkdir()
    (sr.root / "etc/flowerly").symlink_to(decoy)
    baseline = sr.files()
    result = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
    assert result.returncode != 0 and "not a real directory" in result.stderr, result.stderr
    assert sr.files() == baseline and sr.commands() == [] and not any(decoy.iterdir())

    # The lock file replaced by a symlink: refused, the target untouched.
    sr = Sysroot(scratch, "symlink-lock", build)
    victim = sr.root / "etc/important"
    victim.write_text("keep me\n")
    (sr.root / "run").mkdir()
    (sr.root / LOCK).symlink_to(victim)
    result = sr.op("install", "alpha", API_BASE, STYLE, MODEL)
    assert result.returncode != 0 and "is a symlink" in result.stderr, result.stderr
    assert victim.read_text() == "keep me\n" and sr.commands() == []


def case_operator_lock(scratch, build, other_build):
    """MAJOR: one operator command at a time. While a rollback holds the
    lock, install, a second rollback, rollback-shared, preflight and
    accept refuse without any change; the holder then completes."""
    sr = Sysroot(scratch, "lock", build)
    baseline = sr.files()
    sr.install("alpha")
    hold = scratch / "lock-hold"
    holder = subprocess.Popen(["bash", str(OPERATOR), "rollback", "alpha"], env=sr.env(FLOWERLY_OPERATOR_TEST_HOLD=str(hold)),
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        deadline = time.monotonic() + 20
        while not pathlib.Path(f"{hold}.held").exists():
            assert holder.poll() is None, holder.communicate()
            assert time.monotonic() < deadline, "the holder never took the lock"
            time.sleep(0.05)
        before, commands = sr.files(), sr.commands()
        for args in (("install", "beta", API_BASE, STYLE, MODEL), ("rollback", "alpha"), ("rollback-shared",),
                     ("preflight", "alpha"), ("accept", "alpha", "attestation")):
            result = sr.op(*args)
            assert result.returncode != 0 and LOCKED in result.stderr, (args, result.stderr)
            assert sr.files() == before and sr.commands() == commands, args
    finally:
        pathlib.Path(f"{hold}.release").touch()
        out, err = holder.communicate(timeout=60)
    assert holder.returncode == 0, out + err
    assert sr.files() == baseline, set(sr.files()) ^ set(baseline)
    # The lock is released with the process: the next command runs.
    sr.install("beta")


def crash_points(scratch, name, build, prepare, command):
    """Run `command` killed after each change in turn; after every kill the
    documented recovery must restore the baseline exactly, deleting each
    account and group at most once. The one window where the recovery
    cannot prove an account is its own (killed after systemd-sysusers ran,
    before the ledger confirmed it) must report the account instead of
    deleting it; the operator then removes it by hand."""
    point = 0
    ambiguous_points = 0
    while True:
        point += 1
        sr = Sysroot(scratch, f"{name}-{point}", build)
        baseline = sr.files()
        prepare(sr)
        result = sr.op(*command, FLOWERLY_OPERATOR_TEST_CRASH_AFTER=str(point))
        if result.returncode != -signal.SIGKILL:
            assert result.returncode == 0, (point, result.stdout, result.stderr)
            break
        sysusers_ran = any(c.startswith("systemd-sysusers") for c in sr.commands())
        ambiguous = sr.recover()
        if ambiguous:
            assert command[0] == "install" and sysusers_ran, (name, point, ambiguous)
            for kind, account in ambiguous:
                command_ = f"{'userdel' if kind == 'account' else 'groupdel'} {account}"
                assert command_ not in deletions(sr), (name, point, deletions(sr))
            ambiguous_points += 1
            sr.remove_account_lines(ambiguous)
        assert sr.files() == baseline, (name, point, set(sr.files()) ^ set(baseline))
        assert not (sr.root / LEDGER).exists(), (name, point)
        # etc/passwd and etc/group are part of the baseline compared above.
        deleted = deletions(sr)
        assert len(deleted) == len(set(deleted)), (name, point, deleted)
    # At most the kill right after systemd-sysusers and after each of the
    # three account confirmations but the last.
    assert ambiguous_points <= 3, (name, ambiguous_points)
    return point - 1


def case_interrupted(scratch, build, other_build):
    """MAJOR: an interrupted rollback or install, at every change it makes,
    is completed by rerunning the documented commands."""
    def installed(sr):
        sr.install("alpha")
        sr.write("run/flowerly-provider/alpha.sock", "")
    rollback_points = crash_points(scratch, "crash-rollback", build, installed, ("rollback", "alpha"))
    install_points = crash_points(scratch, "crash-install", build, lambda sr: None,
                                  ("install", "alpha", API_BASE, STYLE, MODEL))
    assert rollback_points >= 10 and install_points >= 10, (rollback_points, install_points)
    print(f"interrupted rollback recovered at all {rollback_points} change points, "
          f"interrupted install at all {install_points}")


def case_interrupted_install_spares_foreign(scratch, build, other_build):
    """Review of 807877a, item 2: an install killed at any change, followed
    by another installation creating byte-identical files and the same
    accounts wherever this one had not, then the documented recovery:
    nothing the other installation created is removed or changed."""
    reference = Sysroot(scratch, "foreign-reference", build)
    reference.install("alpha")
    identical = {rel: reference.files()[rel] for rel in SHARED + instance_paths("alpha")}
    accounts = {"passwd": [l for l in (reference.root / "etc/passwd").read_text().splitlines() if l.startswith("flowerly-provider:")],
                "group": [l for l in (reference.root / "etc/group").read_text().splitlines()
                          if l.startswith(("flowerly-provider:", "flowerly-provider-clients:"))]}
    exercised = 0
    # Two other installations: one with its own instance config (which
    # also keeps every shared resource in place), one with only the shared
    # files and accounts (so the account cleanup itself is exercised).
    for variant, candidates in (("all", identical), ("shared", {rel: identical[rel] for rel in SHARED})):
        exercised += spare_foreign(scratch, build, variant, candidates, accounts)
    assert exercised >= 20, exercised
    print(f"interrupted install spared another installation's identical files and accounts at {exercised} points")


def spare_foreign(scratch, build, variant, identical, accounts):
    point, exercised = 0, 0
    while True:
        point += 1
        sr = Sysroot(scratch, f"spare-{variant}-{point}", build)
        result = sr.op("install", "alpha", API_BASE, STYLE, MODEL, FLOWERLY_OPERATOR_TEST_CRASH_AFTER=str(point))
        if result.returncode != -signal.SIGKILL:
            assert result.returncode == 0, result.stderr
            break
        # The other installation fills every gap the killed install left.
        foreign = {}
        for rel, data in identical.items():
            path = sr.root / rel
            if not path.exists() and not path.is_symlink():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
                foreign[rel] = data
        foreign_accounts = []
        for rel, lines in (("etc/passwd", accounts["passwd"]), ("etc/group", accounts["group"])):
            text = (sr.root / rel).read_text()
            for line in lines:
                name = line.split(":")[0]
                if not any(l.split(":")[0] == name for l in text.splitlines()):
                    text += line + "\n"
                    foreign_accounts.append((rel, line))
            (sr.root / rel).write_text(text)
        commands_before = len(sr.commands())
        sr.recover(strict=False)
        for rel, data in foreign.items():
            assert (sr.root / rel).read_bytes() == data, f"point {point}: /{rel} of the other installation was removed"
        for rel, line in foreign_accounts:
            assert line in (sr.root / rel).read_text().splitlines(), f"point {point}: {line.split(':')[0]} was deleted"
            name = line.split(":")[0]
            later = sr.commands()[commands_before:]
            kind = "userdel" if rel == "etc/passwd" else "groupdel"
            assert f"{kind} {name}" not in later, (variant, point, later)
        exercised += bool(foreign or foreign_accounts)
    return exercised


def case_identity_refusals(scratch, build, other_build):
    """An intruder in the socket group, or the provider account in another
    group, makes install refuse with no change."""
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


def case_test_hooks_need_flag(scratch, build, other_build):
    """The test hooks cannot be reached without the explicit test flag."""
    env = dict(os.environ, FLOWERLY_OPERATOR_SYSROOT=str(scratch))
    result = run(["bash", str(OPERATOR), "rollback", "alpha"], env=env, check=False)
    assert result.returncode != 0 and "offline tests only" in result.stderr, result.stderr


ROLLBACK_CASES = [case_single_instance, case_two_instances, case_foreign_shared, case_foreign_conflict,
                  case_other_build, case_other_installation, case_consumer_blocks_all_shared,
                  case_modified_file_keeps_ledger, case_installed_instance_preflight, case_tampered_ledger,
                  case_unknown_instance, case_symlinks, case_operator_lock, case_interrupted,
                  case_interrupted_install_spares_foreign,
                  case_identity_refusals, case_test_hooks_need_flag]


def check_rollback(scratch):
    build, other_build = builds(scratch)
    for case in ROLLBACK_CASES:
        case(scratch, build, other_build)


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
    # The operator refuses directories that group or others can write.
    os.umask(0o022)
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
          f"ledger-scoped rollback ({len(ROLLBACK_CASES)} case groups: usage checks before shared changes, "
          "recoverable partial rollback, operator lock, every interruption point, symlinked parents), "
          "isolated identity, unit probe detection")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <binary_dir>")
    main(pathlib.Path(sys.argv[1]))

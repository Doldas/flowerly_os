#!/usr/bin/env python3
"""Offline tests for the remote provider Arch/KVM acceptance harness (#263).

CI-safe: never starts QEMU/KVM, never needs root, never downloads, never
contacts anything but loopback. These tests check the HARNESS (guardrails,
configuration, evidence rules, the mock, the probe's classification, the
guest runner's fail-closed bookkeeping). They prove nothing about real
kernel or systemd confinement: that needs the guest run, which stays
NOT_RUN until the owner authorizes it.

Covered:
- --help/--plan/--validate/--dry-run run no forbidden command (QEMU, image
  tools, curl, cargo, ssh, gpg, sudo...: PATH shims fail the test if called)
  and say NOT_RUN;
- --execute refuses on each missing gate before any such command: no
  token, wrong or short SHA, dirty tree, CI runner, unpinned or malformed
  image lock, missing keyring;
- the QEMU command line is KVM-only with restrict=on, and no workflow runs
  the execution path;
- the network plan agrees with the operator's DNS classifier (the Rust test
  checks it against providerd's policy) and with the probe targets;
- evidence: NOT_RUN is the only preparation result; PASS without an
  execution, facts or a clean tree, gaps, duplicates, summary drift and
  leaks are rejected;
- the mock's scenarios, DNS answers (including deterministic rebinding)
  and metadata-only log;
- probe classification: refused, timed out, unreachable and uncontrolled
  results never count as an OS block; a seccomp kill (SIGSYS) does;
- the admission client separates refused-at-connect, refused-after-connect
  and admitted;
- the guest runner refuses outside a disposable guest and records harness
  errors as INCONCLUSIVE, unreached checks as NOT_RUN, never PASS.
"""
import hashlib
import http.client
import importlib.util
import json
import os
import pathlib
import re
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
HARNESS = ROOT / "scripts/remote-provider-arch-kvm-acceptance.sh"
TESTBED = ROOT / "testbed/remote-provider-kvm"
GUEST = TESTBED / "guest"
EVIDENCE_TOOL = ROOT / "scripts/remote-provider-kvm-evidence.py"
PROBE = ROOT / "scripts/provider-unit-probe.py"
PLAN_DOC = ROOT / "web-docs/docs/remote-provider-unit-test-plan.md"
FORBIDDEN = ["qemu-system-x86_64", "qemu-img", "qemu-kvm", "curl", "wget", "cargo", "ssh", "scp",
             "cloud-localds", "gpg", "sudo", "su", "ssh-keygen", "systemctl", "systemd-run", "useradd"]
LIVE_HOST_MARKERS = ["gamitronservers", "openui."]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def run(argv, env=None, check=False, cwd=None):
    return subprocess.run([str(a) for a in argv], capture_output=True, text=True, env=env, check=check,
                          cwd=cwd, timeout=120)


# --- guardrails ------------------------------------------------------------

class Shims:
    """Forbidden commands that record any call and fail."""

    def __init__(self, scratch):
        self.dir = scratch / "shims"
        self.dir.mkdir()
        self.log = scratch / "forbidden.log"
        for name in FORBIDDEN:
            shim = self.dir / name
            shim.write_text(f"#!/bin/sh\necho \"{name} $*\" >> '{self.log}'\nexit 99\n")
            shim.chmod(0o755)

    def env(self, **extra):
        env = {k: v for k, v in os.environ.items() if k not in ("GITHUB_ACTIONS", "CI", "FLOWERLY_KVM_ACCEPTANCE")}
        env["PATH"] = f"{self.dir}:{env['PATH']}"
        env.update(extra)
        return env

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []


def scratch_repo(scratch, name="repo"):
    """A committed copy of what the harness reads, so tree state and the
    image lock can be varied without touching this checkout."""
    repo = scratch / name
    for rel in ["scripts/remote-provider-arch-kvm-acceptance.sh", "scripts/remote-provider-kvm-evidence.py",
                "scripts/provider-unit-probe.py", "scripts/remote-provider-acceptance.py",
                "scripts/remote-provider-operator.sh"]:
        (repo / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / rel, repo / rel)
    shutil.copytree(TESTBED, repo / "testbed/remote-provider-kvm",
                    ignore=shutil.ignore_patterns("__pycache__"))
    git = ["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    run(["git", "init", "-q", repo], check=True)
    run([*git, "add", "-A"], check=True)
    run([*git, "commit", "-qm", "fixture"], check=True)
    return repo, run(["git", "-C", repo, "rev-parse", "HEAD"], check=True).stdout.strip()


def commit_lock(repo, lock):
    (repo / "testbed/remote-provider-kvm/arch-image.json").write_text(json.dumps(lock))
    git = ["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
    run([*git, "commit", "-qam", "lock"], check=True)
    return run(["git", "-C", repo, "rev-parse", "HEAD"], check=True).stdout.strip()


FAKE_LOCK = {
    "schema_version": 1, "source_index": "https://geo.mirror.pkgbuild.com/images/",
    "version": "20990101.1",
    "url": "https://geo.mirror.pkgbuild.com/images/v20990101.1/Arch-Linux-x86_64-cloudimg-20990101.1.qcow2",
    "sha256": "0" * 64,
    "signature_url": "https://geo.mirror.pkgbuild.com/images/v20990101.1/Arch-Linux-x86_64-cloudimg-20990101.1.qcow2.sig",
    "signing_key_fingerprint": "0" * 40, "pinned_by": "offline test fixture, not a real pin",
    "pinned_at_utc": "2099-01-01T00:00:00Z"}


def check_preparation_modes(scratch, shims):
    repo, sha = scratch_repo(scratch)
    harness = repo / "scripts/remote-provider-arch-kvm-acceptance.sh"
    for mode in (["--help"], ["--plan"], ["--validate"], ["--dry-run"]):
        result = run(["bash", harness, *mode], env=shims.env())
        assert result.returncode == 0, (mode, result.stdout, result.stderr)
        if mode != ["--help"]:
            assert "NOT_RUN" in result.stdout and sha in result.stdout, (mode, result.stdout)
            assert "UNPINNED" in result.stdout, result.stdout
    out = scratch / "not-run.json"
    result = run(["bash", harness, "--validate", "--evidence-out", out], env=shims.env())
    assert result.returncode == 0, result.stderr
    doc = json.loads(out.read_text())
    assert doc["phase"] == "preparation" and doc["summary"]["overall"] == "NOT_RUN"
    assert {r["test_status"] for r in doc["results"]} == {"NOT_RUN"}
    assert all(r["git_commit"] == sha for r in doc["results"])
    for bad in (["--plan", "--execute"], ["--dry-run", "x"], ["--bogus"], []):
        assert run(["bash", harness, *bad], env=shims.env()).returncode != 0, bad
    assert shims.calls() == [], shims.calls()
    return repo, sha


def check_execute_refusals(scratch, shims, repo, sha):
    harness = repo / "scripts/remote-provider-arch-kvm-acceptance.sh"
    token = {"FLOWERLY_KVM_ACCEPTANCE": "execute-in-disposable-guest"}

    def refused(args, why, **env):
        result = run(["bash", harness, "--execute", *args], env=shims.env(**env))
        assert result.returncode != 0 and why in result.stderr, (args, why, result.stderr)

    refused(["--approved-sha", sha], "FLOWERLY_KVM_ACCEPTANCE")
    refused(["--approved-sha", sha], "FLOWERLY_KVM_ACCEPTANCE", FLOWERLY_KVM_ACCEPTANCE="yes")
    refused([], "usage", **token)
    refused(["--approved-sha", sha[:12]], "full 40-hex", **token)
    refused(["--approved-sha", "f" * 40], "not the approved commit", **token)
    if os.geteuid() != 0:
        refused(["--approved-sha", sha], "image not pinned", **token)
    refused(["--approved-sha", sha], "CI runner", GITHUB_ACTIONS="true", **token)
    refused(["--approved-sha", sha], "CI runner", CI="1", **token)
    (repo / "untracked").write_text("x")
    refused(["--approved-sha", sha], "dirty", **token)
    (repo / "untracked").unlink()
    for broken in ({"url": FAKE_LOCK["url"].replace("v20990101.1/", "latest/")},
                   {"sha256": "abc"}, {"signature_url": "https://example.invalid/x.sig"},
                   {"signing_key_fingerprint": "nothex"}):
        bad_sha = commit_lock(repo, dict(FAKE_LOCK, **broken))
        refused(["--approved-sha", bad_sha], "arch-image.json", **token)
    pinned_sha = commit_lock(repo, FAKE_LOCK)
    if os.geteuid() != 0:
        refused(["--approved-sha", pinned_sha], "FLOWERLY_KVM_ARCH_KEYRING", **token)
        refused(["--approved-sha", pinned_sha], "FLOWERLY_KVM_ARCH_KEYRING",
                FLOWERLY_KVM_ARCH_KEYRING=str(scratch / "absent.gpg"), **token)
    plan = run(["bash", harness, "--plan"], env=shims.env())
    assert "image lock:    pinned" in plan.stdout, plan.stdout
    assert shims.calls() == [], shims.calls()


def check_static_guardrails():
    text = HARNESS.read_text()
    code = "\n".join(line for line in text.splitlines() if not line.lstrip().startswith("#"))
    assert not re.search(r"accel[\s=\"',]*tcg|tcg,thread", code, re.I), "the harness must never fall back to TCG"
    assert "images/latest" not in text and "Arch-Linux-x86_64-cloudimg.qcow2" not in text
    qemu = re.search(r"qemu_args\(\) \{(.*?)\n\}", text, re.S).group(1)
    assert "-accel kvm" in qemu and "restrict=on" in qemu and "hostfwd=tcp:127.0.0.1:" in qemu, qemu
    # Host-side sudo never appears outside commands sent to the guest over ssh.
    for line in code.splitlines():
        if re.search(r"\bsudo\b", line):
            assert "ssh " in line or "sudo:" in line or "|" in line and "ssh" in line, line
    workflows = "\n".join(p.read_text() for p in (ROOT / ".github/workflows").glob("*.yml"))
    assert "remote-provider-arch-kvm-acceptance.sh --execute" not in workflows
    assert "guest-acceptance.py" not in workflows
    for path in [HARNESS, EVIDENCE_TOOL, PROBE, *GUEST.iterdir(), *TESTBED.glob("*.json")]:
        if path.is_file():
            body = path.read_text().lower()
            for marker in LIVE_HOST_MARKERS:
                assert marker not in body, f"{path} names a live endpoint ({marker})"


# --- configuration -----------------------------------------------------------

def check_network_plan():
    plan = json.loads((TESTBED / "network.json").read_text())
    acceptance = load("acceptance", ROOT / "scripts/remote-provider-acceptance.py")
    assert plan["domain"].endswith(".test") and all(n.endswith("." + plan["domain"]) for n in plan["names"].values())
    assert set(plan["addresses"]) == set(plan["expected_policy"])
    for role, address in plan["addresses"].items():
        verdict = acceptance.classify(address)
        want = "public" if plan["expected_policy"][role] == "allowed" else "refused"
        assert verdict == want, (role, address, verdict)
    os_policy = plan["unit_os_policy"]
    assert set(os_policy["blocked"]) | set(os_policy["not_blocked"]) == set(plan["addresses"])
    targets = json.loads((TESTBED / "probe-targets.json").read_text())
    addresses = set(plan["addresses"].values())
    for target in targets["connect_blocked"] + targets["connect_not_blocked"]:
        assert target["address"] in addresses, target
    blocked = {plan["addresses"][r] for r in os_policy["blocked"]}
    assert {t["address"] for t in targets["connect_blocked"]} == blocked
    assert targets["dns_stub"] == plan["dns_stub"]
    lock = json.loads((TESTBED / "arch-image.json").read_text())
    assert lock["sha256"] is None and lock["version"] is None, "the image must stay unpinned until a reviewed pin"


# --- evidence -----------------------------------------------------------------

def check_evidence():
    ev = load("evidence", EVIDENCE_TOOL)
    catalog = ev.catalog()
    ids = [c["test_id"] for c in catalog]
    assert len(ids) == len(set(ids)) and {c["exercises"] for c in catalog} == {"production", "harness"}
    plan_text = PLAN_DOC.read_text()
    missing = [i for i in ids if not re.search(rf"\b{i}\b", plan_text)]
    assert not missing, f"plan document does not describe {missing}"
    sha = "a" * 40
    doc = ev.not_run(sha, True, "preparation only")
    assert ev.validate(doc, "preparation") == [] and doc["summary"]["overall"] == "NOT_RUN"
    try:
        import jsonschema  # optional: CI may not have it
        jsonschema.validate(doc, json.loads((TESTBED / "evidence.schema.json").read_text()))
    except ImportError:
        pass

    def broken(mutate, phase=None):
        bad = json.loads(json.dumps(doc))
        mutate(bad)
        problems = ev.validate(bad, phase)
        assert problems, "mutation accepted"
        return problems

    def fake_pass(d):
        d["results"][0].update(test_status="PASS", observed={"x": 1}, skip_reason=None)
        d["summary"] = ev.summarize(d["phase"], d["results"])
    assert any("only an executed run" in p for p in broken(fake_pass))

    def exec_pass_without_facts(d):
        d["phase"] = "execution"
        fake_pass(d)
    assert any("PASS without" in p for p in broken(exec_pass_without_facts))

    def all_pass_dirty(d):
        d["phase"] = "execution"
        for r in d["results"]:
            r.update(test_status="PASS", observed={"ok": True}, expected="x", skip_reason=None, git_tree_clean=False,
                     guest_image="img", guest_image_sha256="b" * 64, guest_kernel="k", systemd_version="s",
                     providerd_sha256="c" * 64, unit_sha256="d" * 64, test_started_at_utc="2099-01-01T00:00:00Z",
                     test_finished_at_utc="2099-01-01T00:00:01Z")
        d["summary"] = ev.summarize("execution", d["results"])
    assert any("dirty tree" in p for p in broken(all_pass_dirty))
    good = json.loads(json.dumps(doc))
    all_pass_dirty(good)
    for r in good["results"]:
        r["git_tree_clean"] = True
    good["summary"] = ev.summarize("execution", good["results"])
    assert ev.validate(good, "execution") == [] and good["summary"]["overall"] == "PASS"
    broken(lambda d: d["results"].pop())
    broken(lambda d: d["results"].append(dict(d["results"][0])))
    broken(lambda d: d["results"][0].update(test_status="SKIPPED"))
    broken(lambda d: d["results"][0].update(skip_reason=None))
    broken(lambda d: d["results"][0].update(observed={"something": 1}))
    broken(lambda d: d["results"][0].update(exercises="harness" if d["results"][0]["exercises"] == "production"
                                            else "production"))
    broken(lambda d: d["summary"].update(overall="PASS"))
    broken(lambda d: d["results"][1].update(git_commit="b" * 40))
    broken(lambda d: d["results"][0].update(extra=1))
    for leak in ["Bearer abcdef", "Authorization: x", "sk-abcdefghijkl", "-----BEGIN PRIVATE KEY-----",
                 "FLOWERLY-SENTINEL-PROMPT-x", "flowerly-kvm-dummy-key-1", "FLOWERLY-SENTINEL-REPLY"]:
        assert any("leak" in p for p in broken(lambda d, leak=leak: d["results"][0].update(skip_reason=leak))), leak
    assert ev.validate(doc, "execution"), "a preparation document must not pass as an execution"


# --- mock ---------------------------------------------------------------------

def dns_query(port, name, qtype=1):
    question = b"".join(bytes([len(p)]) + p.encode() for p in name.split(".")) + b"\0" + struct.pack("!HH", qtype, 1)
    packet = struct.pack("!HHHHHH", 0x1234, 0x0100, 1, 0, 0, 0) + question
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.settimeout(5)
        sock.sendto(packet, ("127.0.0.1", port))
        reply = sock.recv(512)
    rcode = struct.unpack("!H", reply[2:4])[0] & 0xF
    count = struct.unpack("!H", reply[6:8])[0]
    if not count:
        return rcode, None
    rdata = reply[-(4 if qtype == 1 else 16):]
    return rcode, socket.inet_ntop(socket.AF_INET if qtype == 1 else socket.AF_INET6, rdata)


def check_mock(scratch):
    pki = scratch / "pki"
    pki.mkdir()
    plan = json.loads((TESTBED / "network.json").read_text())
    names = plan["names"]
    run(["bash", GUEST / "make-test-pki.sh", pki, names["mock"], names["redirect_target"], names["wrong_name"]],
        check=True)
    assert not list(pki.glob("*ca.key")), "CA keys must not outlive signing"
    state = scratch / "mock-state"
    state.mkdir()
    dummy = "flowerly-kvm-dummy-key-offline-test"
    proc = subprocess.Popen([sys.executable, "-I", GUEST / "mock-provider.py", "--network", TESTBED / "network.json",
                             "--pki", pki, "--state", state, "--model", "gpt-oss:20b",
                             "--key-sha256", hashlib.sha256(dummy.encode()).hexdigest(),
                             "--listen", "mock=127.0.0.1:0", "--listen", "redirect_target=127.0.0.1:0",
                             "--dns", "127.0.0.1:0"])
    try:
        for _ in range(100):
            if (state / "ports.json").exists():
                break
            time.sleep(0.05)
        ports = json.loads((state / "ports.json").read_text())
        mock_port, dns_port = ports["mock"][1], ports["dns"][1]

        def scenario(word, *extra):
            (state / "scenario").write_text(" ".join([word, str(time.monotonic_ns()), *extra]))

        def https(method, path, body=None, auth=dummy, ca="test-ca.crt", name=names["mock"]):
            ctx = ssl.create_default_context(cafile=str(pki / ca))
            conn = http.client.HTTPSConnection("127.0.0.1", mock_port, context=ctx, timeout=20)
            conn.sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", mock_port), timeout=20),
                                        server_hostname=name)
            headers = {"Content-Type": "application/json"}
            if auth:
                headers["Authorization"] = f"Bearer {auth}"
            conn.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
            response = conn.getresponse()
            try:
                data = response.read()
            except http.client.IncompleteRead as err:
                data = ("incomplete", len(err.partial))
            return response.status, dict(response.getheaders()), data

        chat = {"model": "gpt-oss:20b", "max_tokens": 16, "stream": False,
                "messages": [{"role": "user", "content": "FLOWERLY-SENTINEL-PROMPT-offline: hi"}]}
        scenario("ok")
        status, _, data = https("GET", "/api/models")
        assert status == 200 and any(m["id"] == "gpt-oss:20b" for m in json.loads(data)["data"])
        status, _, data = https("POST", "/api/chat/completions", chat)
        reply = json.loads(data)
        assert status == 200 and reply["model"] == "gpt-oss:20b" and reply["usage"]["completion_tokens"] == 3
        assert reply["choices"][0]["message"]["content"] == "FLOWERLY-SENTINEL-REPLY"
        assert https("GET", "/v1/models", auth="wrong")[0] == 401
        assert https("GET", "/v1/models", auth=None)[0] == 401
        expectations = {
            "auth-failure": lambda s, h, d: s == 401,
            "rate-limited": lambda s, h, d: s == 429 and h.get("Retry-After") == "7",
            "server-error": lambda s, h, d: s == 503,
            "redirect": lambda s, h, d: s == 302 and h["Location"].startswith(f"https://{names['redirect_target']}:"),
            "oversized": lambda s, h, d: s == 200 and int(h["Content-Length"]) > 1024 * 1024,
            "malformed-json": lambda s, h, d: s == 200 and not _parses(d),
            "tool-calls": lambda s, h, d: json.loads(d)["choices"][0]["message"]["tool_calls"],
            "missing-usage": lambda s, h, d: "usage" not in json.loads(d),
            "model-mismatch": lambda s, h, d: json.loads(d)["model"] == "mock-other-model",
            "empty-output": lambda s, h, d: json.loads(d)["choices"][0]["message"]["content"] == "",
            "unknown-outcome": lambda s, h, d: s == 200 and isinstance(d, tuple),
        }
        for word, ok in expectations.items():
            scenario(word)
            result = https("POST", "/v1/chat/completions", chat)
            assert ok(*result), (word, result[0], result[1])
        scenario("missing-model")
        assert all(m["id"] != "gpt-oss:20b" for m in json.loads(https("GET", "/v1/models")[2])["data"])
        scenario("delayed")
        started = time.monotonic()
        assert https("POST", "/v1/chat/completions", chat)[0] == 200 and time.monotonic() - started >= 5
        for word, ca, name in [("tls-untrusted", "test-ca.crt", names["mock"]),
                               ("tls-wrong-name", "test-ca.crt", names["mock"])]:
            scenario(word)
            try:
                https("GET", "/v1/models", ca=ca, name=name)
                raise AssertionError(f"{word}: TLS verification passed")
            except ssl.SSLCertVerificationError:
                pass
        # DNS: public by default, the scenario's address otherwise, TTL-0
        # rebinding advanced by A queries only, NXDOMAIN outside the plan.
        addresses = plan["addresses"]
        scenario("ok")
        assert dns_query(dns_port, names["mock"]) == (0, addresses["public_mock"])
        assert dns_query(dns_port, names["mock"], 28) == (0, None)
        assert dns_query(dns_port, names["redirect_target"]) == (0, addresses["redirect_target"])
        assert dns_query(dns_port, "real.example.com")[0] == 3
        for word, role in [("addr-private", "private_rfc1918"), ("addr-cgnat", "cgnat"),
                           ("addr-loopback", "loopback_v4"), ("addr-metadata", "metadata_v4")]:
            scenario(word)
            assert dns_query(dns_port, names["mock"]) == (0, addresses[role]), word
        scenario("addr-ula")
        assert dns_query(dns_port, names["mock"], 28) == (0, addresses["ula"])
        scenario("rebind", "1")
        sequence = []
        for qtype in (1, 28, 28, 1, 1):
            answer = dns_query(dns_port, names["mock"], qtype)[1]
            if qtype == 1:
                sequence.append(answer)
        assert sequence == [addresses["public_mock"], addresses["private_rfc1918"], addresses["private_rfc1918"]]
        scenario("rebind", "2")
        assert [dns_query(dns_port, names["mock"])[1] for _ in range(3)] == \
            [addresses["public_mock"]] * 2 + [addresses["private_rfc1918"]]
        log = (state / "events.jsonl").read_text()
        for secret in (dummy, "FLOWERLY-SENTINEL-PROMPT-offline", "FLOWERLY-SENTINEL-REPLY", "Bearer"):
            assert secret not in log, f"mock log contains {secret!r}"
        events = [json.loads(line) for line in log.splitlines()]
        assert any(e.get("sentinel_prompt_seen") for e in events)
        assert {e["auth"] for e in events if e.get("kind") == "http" and "auth" in e} >= {"match", "mismatch", "absent"}
    finally:
        proc.terminate()
        proc.wait(timeout=10)


def _parses(data):
    try:
        json.loads(data)
        return True
    except ValueError:
        return False


# --- probe --------------------------------------------------------------------

def check_probe(scratch):
    probe = load("probe", PROBE)
    assert probe.classify_connect("ECONNREFUSED") == "connection_refused"
    assert probe.classify_connect("ETIMEDOUT") == "timed_out"
    assert probe.classify_connect("ENETUNREACH") == "inconclusive"
    assert probe.classify_connect("EPERM") == "blocked_by_os"
    assert probe.classify_denial("ENOENT", probe.BLOCKED_ERRNOS, False) == "inconclusive"
    assert probe.classify_denial("ENOENT", probe.BLOCKED_ERRNOS, True) == "hidden_by_os"
    results = {}
    probe.add(results, "uncontrolled", "blocked_by_os", "EPERM", "blocked_by_os", control="ECONNREFUSED",
              controlled=True)
    assert results["uncontrolled"]["classification"] == "inconclusive" and not results["uncontrolled"]["ok"]
    probe.add(results, "controlled", "blocked_by_os", "EPERM", "blocked_by_os", control="allowed", controlled=True)
    assert results["controlled"]["ok"]
    # A seccomp kill of the forked attempt counts as an OS block.
    import signal
    assert probe.in_child(lambda: os.kill(os.getpid(), signal.SIGSYS)) == "SIGSYS"
    assert probe.classify_denial("SIGSYS", probe.BLOCKED_ERRNOS, False) == "blocked_by_os"
    # A refusing listener and a silent address, unconfined.
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    port = listener.getsockname()[1]
    listener.close()
    targets = scratch / "targets.json"
    targets.write_text(json.dumps({"read_denied": [str(scratch / "absent-file")], "write_denied": [],
                                   "connect_blocked": [{"name": "refused", "address": "127.0.0.1", "port": port}],
                                   "connect_not_blocked": [], "dns_stub": None}))
    result = run([sys.executable, "-I", PROBE, "--targets", targets])
    report = json.loads(result.stdout)
    assert result.returncode == 1
    assert report["checks"]["connect refused"]["classification"] == "connection_refused"
    assert "connect refused" in report["violations"]
    assert f"read {scratch / 'absent-file'}" in report["inconclusive"]
    control = scratch / "control.json"
    result = run([sys.executable, "-I", PROBE, "--targets", targets, "--control-out", control])
    assert result.returncode == 1 and "connect refused" in json.loads(control.read_text())["unreachable"]
    result = run([sys.executable, "-I", PROBE, "--targets", targets, "--control", control])
    report = json.loads(result.stdout)
    assert result.returncode == 1 and report["controlled"] and "connect refused" in report["inconclusive"]


# --- socket client ---------------------------------------------------------------

def check_admission_client(scratch):
    """The three admission outcomes the socket matrix relies on."""
    import threading
    client = GUEST / "provider-client.py"

    def admission(path):
        return json.loads(run([sys.executable, "-I", client, "admission", path]).stdout)

    absent = admission(scratch / "absent.sock")
    assert absent["admitted"] is False and "connect_error" in absent, absent

    def serve(path, answer):
        server = socket.socket(socket.AF_UNIX)
        server.bind(str(path))
        server.listen(1)

        def loop():
            conn, _ = server.accept()
            with conn:
                if answer:
                    size = struct.unpack("<I", conn.recv(4))[0]
                    request = json.loads(conn.recv(size))
                    reply = json.dumps({"version": 1, "id": request["id"], "type": "error", "code": "not_found",
                                        "message": "unknown request [unknown_request; outcome not_sent]"}).encode()
                    conn.sendall(struct.pack("<I", len(reply)) + reply)
            server.close()
        thread = threading.Thread(target=loop, daemon=True)
        thread.start()
        return thread

    thread = serve(scratch / "closing.sock", answer=False)
    closed = admission(scratch / "closing.sock")
    thread.join(5)
    assert closed["admitted"] is False and "refused_after_connect" in closed, closed
    thread = serve(scratch / "answering.sock", answer=True)
    admitted = admission(scratch / "answering.sock")
    thread.join(5)
    assert admitted["admitted"] is True and admitted["reply"]["class"] == "unknown_request", admitted
    assert set(admitted["identity"]) == {"Uid", "Gid", "Groups"}


# --- guest runner --------------------------------------------------------------

def check_guest_runner(scratch):
    if os.geteuid() != 0:
        result = run([sys.executable, "-I", GUEST / "guest-acceptance.py", scratch])
        assert result.returncode != 0 and "run as root inside the disposable guest" in result.stderr, result.stderr
    bundle = scratch / "bundle"
    bundle.mkdir()
    (bundle / "repo").symlink_to(ROOT)
    (bundle / "run.json").write_text(json.dumps({"run_id": "offline-test-run", "git_commit": "e" * 40,
                                                 "git_tree_clean": True}))
    guest = load("guest", GUEST / "guest-acceptance.py")
    guest.WORK = scratch / "work"
    runner = guest.Run(bundle)

    def boom():
        raise guest.Harness("simulated harness failure")
    assert runner.check("A1", "x", boom) is False
    assert runner.results["A1"]["test_status"] == "INCONCLUSIVE"
    assert runner.check("A2", "x", lambda: 1 / 0) is False
    assert runner.results["A2"]["test_status"] == "INCONCLUSIVE"
    assert runner.check("A3", "x", lambda: (False, {"seen": 1})) is False
    assert runner.results["A3"]["test_status"] == "FAIL"
    assert runner.check("A4", "x", lambda: ("yes", {"seen": 1})) is False, "only True passes"
    runner.check("A5", "x", lambda: (True, {"seen": 1}))
    assert runner.results["A5"]["test_status"] == "PASS"
    status = runner.write()
    assert status == 1
    written = list((scratch / "work/evidence").glob("*.json"))
    assert len(written) == 1, written
    doc = json.loads(written[0].read_text())
    if doc.get("invalid"):
        # PASS without guest facts must be refused by the validator.
        assert any("PASS without" in p for p in doc["problems"]), doc["problems"]
    else:
        raise AssertionError("evidence with a fact-less PASS was accepted")


def main():
    with tempfile.TemporaryDirectory(prefix="fl-kvm-", dir="/tmp") as scratch:
        scratch = pathlib.Path(scratch)
        shims = Shims(scratch)
        repo, sha = check_preparation_modes(scratch, shims)
        check_execute_refusals(scratch, shims, repo, sha)
        check_static_guardrails()
        check_network_plan()
        check_evidence()
        check_mock(scratch)
        check_probe(scratch)
        check_admission_client(scratch)
        check_guest_runner(scratch)
        assert shims.calls() == [], shims.calls()
    print("remote provider KVM harness, offline: preparation modes run nothing forbidden and report NOT_RUN; "
          "--execute refuses on every missing gate; KVM-only, restrict=on, no workflow executes it; "
          "network plan matches the DNS classifier and probe targets; evidence rules reject fabricated "
          "PASS, gaps and leaks; mock scenarios, DNS and rebinding; probe classification; guest runner "
          "fail-closed. (Harness only: real confinement remains NOT_RUN.)")


if __name__ == "__main__":
    main()

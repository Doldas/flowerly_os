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
  errors as INCONCLUSIVE, unreached checks as NOT_RUN, never PASS;
- E2: simulated non-isolated configurations that the a505c87 logic passed
  (refused or silent probes, no route) now FAIL or are INCONCLUSIVE; only
  restrict=on read back from QEMU plus a live, untouched host canary passes;
- evidence chain: only a bound record whose commit, run id, image, binary
  and unit hashes, tree state and host-finalized E1-E3 all agree verifies
  as PASS; preparation, partial, edited or mismatched runs never do;
- SSH uses a per-run pinned guest host key;
- image trust (#263 review): the lock pins a primary key and approved
  signing subkeys; the harness checks the keyring before any download and
  the image (scripts/arch-image-verify.py, tested with real GnuPG in
  scripts/test-arch-image-verify.py) before any boot, never calling gpg
  itself; E3 passes only when the recorded digest and signer identities
  agree with the pin, and image facts that claim a verification their
  identities contradict make the record INVALID;
- end to end through the real guest Run.check/check_environment/write:
  E1-E3 await the host and a ready guest exits 0; valid host facts PASS;
  invalid host facts FAIL, stay INCONCLUSIVE or are INVALID; genuine guest
  failures stay failures; invalid verdicts and malformed or missing
  evidence never PASS.
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
                "scripts/arch-image-verify.py",
                "scripts/provider-unit-probe.py", "scripts/remote-provider-acceptance.py",
                "scripts/remote-provider-operator.sh"]:
        (repo / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / rel, repo / rel)
    shutil.copytree(TESTBED, repo / "testbed/remote-provider-kvm",
                    ignore=shutil.ignore_patterns("__pycache__"))
    # The guardrail tests start from an unpinned lock whatever the real one
    # holds; they pin their own fake lock where they need one.
    lock_path = repo / "testbed/remote-provider-kvm/arch-image.json"
    lock_path.write_text(json.dumps(dict(json.loads(lock_path.read_text()),
                                         **{f: None for f in LOCK_FIELDS}), indent=2) + "\n")
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


LOCK_FIELDS = ("version", "url", "sha256", "signature_url", "primary_key_fingerprint",
               "signing_subkey_fingerprints", "pinned_by", "pinned_at_utc")
FAKE_LOCK = {
    "schema_version": 1, "source_index": "https://geo.mirror.pkgbuild.com/images/",
    "version": "20990101.1",
    "url": "https://geo.mirror.pkgbuild.com/images/v20990101.1/Arch-Linux-x86_64-cloudimg-20990101.1.qcow2",
    "sha256": "0" * 64,
    "signature_url": "https://geo.mirror.pkgbuild.com/images/v20990101.1/Arch-Linux-x86_64-cloudimg-20990101.1.qcow2.sig",
    "primary_key_fingerprint": "0" * 40, "signing_subkey_fingerprints": ["1" * 40],
    "pinned_by": "offline test fixture, not a real pin",
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
                   {"primary_key_fingerprint": "nothex"}, {"signing_subkey_fingerprints": "1" * 40},
                   {"signing_subkey_fingerprints": ["1" * 40, "1" * 40]},
                   {"signing_subkey_fingerprints": ["0" * 40]}, {"signing_subkey_fingerprints": ["abc"]}):
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
    # The guest's SSH host key is generated per run and pinned: no TOFU.
    assert "StrictHostKeyChecking=no" not in code and "UserKnownHostsFile=/dev/null" not in code
    assert code.count("StrictHostKeyChecking=yes") >= 2 and "HostKeyAlias=" in code
    assert "ssh_deletekeys: true" in text and "ed25519_private" in text
    # Host-side sudo never appears outside commands sent to the guest over ssh.
    for line in code.splitlines():
        if re.search(r"\bsudo\b", line):
            assert "ssh " in line or "sudo:" in line or "|" in line and "ssh" in line, line
    # Image trust: never a direct gpg call or a human-readable verdict; the
    # keyring is checked before any download, the image before any boot.
    assert not re.search(r"^\s*[^#]*\bgpg\s", "\n".join(l for l in code.splitlines() if "HOST_TOOLS" not in l), re.M)
    gate = re.search(r"require_execution_gates\(\) \{(.*?)\n\}", text, re.S).group(1)
    assert '"$IMAGE_TOOL" keyring' in gate, "the keyring structure must be checked before execution starts"
    body = re.search(r"\nexecute\(\) \{(.*?)\n\}", text, re.S).group(1)
    assert body.index('"$IMAGE_TOOL" verify') < body.index("qemu-img create") < body.index("qemu-system-x86_64")
    assert body.index("curl") < body.index('"$IMAGE_TOOL" verify')
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
    # The real lock is either wholly unpinned or wholly pinned and accepted
    # by the harness; never half-pinned.
    lock = json.loads((TESTBED / "arch-image.json").read_text())
    unset = [f for f in LOCK_FIELDS if lock.get(f) in (None, "", [])]
    assert unset in ([], list(LOCK_FIELDS)), f"arch-image.json is half-pinned: {unset} unset"
    if not unset:
        plan = run(["bash", HARNESS, "--plan"])
        assert "image lock:    pinned" in plan.stdout, plan.stdout


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


# --- E2 egress and the host/guest evidence chain --------------------------------

def old_e2_passed(attempts):
    """E2 as it was at a505c87: PASS when no probe connected."""
    return all(v != "connected" for v in attempts.values())


RESTRICTED = ["qemu-system-x86_64", "-machine", "q35", "-accel", "kvm", "-cpu", "host",
              "-nic", "user,model=virtio-net-pci,restrict=on,hostfwd=tcp:127.0.0.1:40222-:22"]
UNRESTRICTED = [a.replace("restrict=on,", "") for a in RESTRICTED]
LIVE_CANARY = {"live_before": True, "live_after": True, "control_connections": 2, "total_connections": 2}
GUEST_OK = {"gateway": "10.0.2.2", "default_route_via": "10.0.2.2", "local_control": "connected",
            "canary_attempt": "ETIMEDOUT", "canary_port": 40001}


def e2_outcome(guest, ev, guest_eval, qemu, canary):
    """Full E2 decision: the guest's half, then the host finalization."""
    status, cls = guest_eval(guest)
    record = {"test_id": "E2", "test_status": status, "classification": cls, "observed": guest,
              "skip_reason": None, "test_started_at_utc": None}
    doc = ev.finalize({"phase": "execution", "results": [record]},
                      {"qemu": {"cmdline": qemu}, "egress_canary": canary, "finished_at_utc": "2099-01-01T00:00:00Z"})
    return doc["results"][0]["test_status"], doc["results"][0]["classification"]


def check_e2(ev, guest):
    evaluate = guest.evaluate_guest_egress
    # The guest alone never passes E2.
    assert evaluate(GUEST_OK) == ("INCONCLUSIVE", "awaiting_host")
    scenarios = [
        # (name, guest observations, QEMU command line, canary, expected status)
        ("isolated", GUEST_OK, RESTRICTED, LIVE_CANARY, "PASS"),
        # Not isolated, nothing listening behind the gateway: every guest
        # probe is refused. a505c87 called this PASS.
        ("unrestricted, refused probes", dict(GUEST_OK, canary_attempt="ECONNREFUSED"), UNRESTRICTED,
         LIVE_CANARY, "FAIL"),
        ("unrestricted, silent probes", GUEST_OK, UNRESTRICTED, LIVE_CANARY, "FAIL"),
        ("canary reached, guest saw a timeout", GUEST_OK, RESTRICTED, dict(LIVE_CANARY, total_connections=3),
         "FAIL"),
        ("guest connected", dict(GUEST_OK, canary_attempt="connected"), RESTRICTED, LIVE_CANARY, "FAIL"),
        ("no default route", dict(GUEST_OK, default_route_via=None), RESTRICTED, LIVE_CANARY, "INCONCLUSIVE"),
        ("route elsewhere", dict(GUEST_OK, default_route_via="192.168.1.1"), RESTRICTED, LIVE_CANARY,
         "INCONCLUSIVE"),
        ("probe method unproven", dict(GUEST_OK, local_control="ECONNREFUSED"), RESTRICTED, LIVE_CANARY,
         "INCONCLUSIVE"),
        ("canary dead before", GUEST_OK, RESTRICTED, dict(LIVE_CANARY, live_before=False), "INCONCLUSIVE"),
        ("canary dead after", GUEST_OK, RESTRICTED, dict(LIVE_CANARY, live_after=False), "INCONCLUSIVE"),
        ("no canary attempt", dict(GUEST_OK, canary_attempt=None), RESTRICTED, LIVE_CANARY, "INCONCLUSIVE"),
        ("no QEMU command line", GUEST_OK, [], LIVE_CANARY, "INCONCLUSIVE"),
        ("extra NIC", GUEST_OK, RESTRICTED + ["-nic", "user"], LIVE_CANARY, "FAIL"),
        ("guestfwd", GUEST_OK, [a + ",guestfwd=tcp:10.0.2.100:80-cmd:x" if a.startswith("user,") else a
                                for a in RESTRICTED], LIVE_CANARY, "FAIL"),
        ("forward on all interfaces", GUEST_OK, [a.replace("127.0.0.1", "") for a in RESTRICTED], LIVE_CANARY,
         "FAIL"),
    ]
    for name, obs, qemu, canary, want in scenarios:
        got = e2_outcome(obs, ev, evaluate, qemu, canary)[0]
        assert got == want, (name, got, want)
    # The regression: these simulated NON-isolated configurations satisfied
    # the old E2 (no probe connected), and the corrected logic rejects them.
    for name, attempt in [("refused", "ECONNREFUSED"), ("timeout", "ETIMEDOUT")]:
        old = {"host_gateway_ssh": attempt, "slirp_dns_tcp": attempt, "documentation_address": "ETIMEDOUT"}
        assert old_e2_passed(old), "the old logic must be shown to pass this"
        status = e2_outcome(dict(GUEST_OK, canary_attempt=attempt), ev, evaluate, UNRESTRICTED, LIVE_CANARY)[0]
        assert status == "FAIL", (name, status)
    old_no_route = {"host_gateway_ssh": "ENETUNREACH", "slirp_dns_tcp": "ENETUNREACH",
                    "documentation_address": "ENETUNREACH"}
    assert old_e2_passed(old_no_route)
    assert e2_outcome(dict(GUEST_OK, default_route_via=None, canary_attempt="ENETUNREACH"), ev, evaluate,
                      RESTRICTED, LIVE_CANARY)[0] == "INCONCLUSIVE"
    # E1: KVM only; TCG anywhere fails.
    assert ev.qemu_accel_kvm(RESTRICTED)
    assert not ev.qemu_accel_kvm([a.replace("kvm", "tcg") for a in RESTRICTED])
    assert not ev.qemu_accel_kvm(RESTRICTED + ["-accel", "tcg"])
    assert not ev.qemu_accel_kvm(["qemu-system-x86_64"])
    # The harness's own QEMU line satisfies both checks.
    line = run(["bash", "-c", f"source <(sed -n '/^qemu_args() {{/,/^}}/p' {HARNESS}); "
                              "VM_CPUS=4 VM_MEMORY_MB=4096; qemu_args r /run img 40222"], check=True).stdout.split()
    assert ev.qemu_accel_kvm(line) and ev.qemu_network_restricted(line), line


PRIMARY, SUBKEY, OTHER_SUBKEY = "A" * 40, "B" * 40, "C" * 40


def verified_image(image_sha):
    """Host image facts as scripts/arch-image-verify.py records a good
    verification: an approved subkey of the pinned primary signed it."""
    return {"lock_sha256": image_sha, "sha256_expected": image_sha, "sha256_actual": image_sha,
            "lock_primary_fingerprint": PRIMARY, "lock_signing_subkeys": [SUBKEY],
            "keyring_primary_fingerprints": [PRIMARY], "keyring_subkey_fingerprints": [SUBKEY, OTHER_SUBKEY],
            "signature_primary_fingerprint": PRIMARY, "signature_signing_fingerprint": SUBKEY,
            "signature_status": ["NEWSIG", "GOODSIG", "VALIDSIG"], "signature_verified": True,
            "verification_problems": []}


def accepted_run(ev):
    """A complete, consistent synthetic run: guest document and host facts."""
    commit, image_sha = "c" * 40, "d" * 64
    facts = {"git_commit": commit, "git_tree_clean": True, "guest_image": "Arch-Linux-x86_64-cloudimg-x",
             "guest_image_sha256": image_sha, "guest_kernel": "6.x-arch", "systemd_version": "systemd 999",
             "providerd_sha256": "e" * 64, "unit_sha256": "f" * 64}
    results = []
    for check in ev.catalog():
        if check["test_id"] in ("E1", "E2", "E3"):
            obs = dict(GUEST_OK) if check["test_id"] == "E2" else {"guest_virt": "kvm"}
            results.append(ev.record(check, facts, "INCONCLUSIVE", expected="x", observed=obs,
                                     classification="awaiting_host", started="2099-01-01T00:00:00Z",
                                     finished="2099-01-01T00:00:00Z"))
        else:
            results.append(ev.record(check, facts, "PASS", expected="x", observed={"ok": True},
                                     started="2099-01-01T00:00:00Z", finished="2099-01-01T00:00:01Z"))
    guest_doc = ev.document("execution", "synthetic-run-0001", results)
    host = {"run_id": "synthetic-run-0001", "approved_commit": commit, "head_at_start": commit, "head_at_end": commit,
            "tree_clean_at_start": True, "tree_clean_at_end": True, "finished_at_utc": "2099-01-01T01:00:00Z",
            "image": verified_image(image_sha),
            "qemu": {"cmdline": RESTRICTED}, "egress_canary": dict(LIVE_CANARY),
            "build": {"providerd_sha256": "e" * 64, "flowerlyd_sha256": "0" * 64, "unit_sha256": "f" * 64}}
    return guest_doc, host


# Each leaves signature_verified true while the recorded identities or
# digest say otherwise: a tampered or inconsistent record, never a PASS.
IMAGE_CONTRADICTIONS = {
    "signer is another primary's key": lambda h: h["image"].update(signature_primary_fingerprint="D" * 40),
    "signer is an unapproved subkey": lambda h: h["image"].update(signature_signing_fingerprint=OTHER_SUBKEY),
    "signer is the primary itself": lambda h: h["image"].update(signature_signing_fingerprint=PRIMARY),
    "signer missing": lambda h: h["image"].update(signature_signing_fingerprint=None),
    "extra primary in keyring": lambda h: h["image"].update(keyring_primary_fingerprints=[PRIMARY, "D" * 40]),
    "keyring primary is another key": lambda h: h["image"].update(keyring_primary_fingerprints=["D" * 40]),
    "approved subkey not in keyring": lambda h: h["image"].update(keyring_subkey_fingerprints=[OTHER_SUBKEY]),
    "lock lists the primary as a subkey": lambda h: h["image"].update(lock_signing_subkeys=[PRIMARY]),
    "lock subkeys empty": lambda h: h["image"].update(lock_signing_subkeys=[]),
    "lock primary lowercase": lambda h: h["image"].update(lock_primary_fingerprint="a" * 40),
    "problems recorded": lambda h: h["image"].update(verification_problems=["GnuPG reported EXPKEYSIG"]),
    "problems missing": lambda h: h["image"].pop("verification_problems"),
    "digest differs": lambda h: h["image"].update(sha256_actual="9" * 64),
    "digest missing": lambda h: h["image"].update(sha256_actual=None, sha256_expected=None, lock_sha256=None),
    "old single-fingerprint facts": lambda h: h.update(image={
        k: h["image"][k] for k in ("lock_sha256", "sha256_expected", "sha256_actual", "signature_verified")}),
}


def check_binding(ev, scratch):
    guest_doc, host = accepted_run(ev)
    # The guest's own document is never an acceptance: E1-E3 await the host.
    assert guest_doc["summary"]["overall"] == "INCONCLUSIVE"
    bound = ev.bind(guest_doc, host)
    assert bound["verdict"] == "PASS" and bound["problems"] == [], bound["problems"]
    path = scratch / "acceptance.json"
    path.write_text(json.dumps(bound))
    assert run([sys.executable, "-I", EVIDENCE_TOOL, "verify-bound", path]).returncode == 0

    def verdict(mutate_guest=None, mutate_host=None, mutate_bound=None):
        g, h = accepted_run(ev)
        if mutate_guest:
            mutate_guest(g)
            g["summary"] = ev.summarize(g["phase"], g["results"])
        if mutate_host:
            mutate_host(h)
        b = ev.bind(g, h)
        if mutate_bound:
            mutate_bound(b)
            b["verdict"], b["problems"] = ev.verify_bound(b)
        return b["verdict"]

    def by_id(doc, test_id):
        return next(r for r in doc["results"] if r["test_id"] == test_id)
    cases = {
        "preparation document": (dict(mutate_bound=lambda b: b.update(
            guest_evidence=ev.not_run("c" * 40, True, "prep"))), "INVALID"),
        "partial run": (dict(mutate_guest=lambda g: by_id(g, "C16").update(
            test_status="NOT_RUN", observed=None, skip_reason="not reached")), "NOT_RUN"),
        "a failed check": (dict(mutate_guest=lambda g: by_id(g, "A10").update(test_status="FAIL")), "FAIL"),
        "guest claims E2 itself": (dict(mutate_guest=lambda g: by_id(g, "E2").update(
            test_status="PASS", classification="x")), "INCONCLUSIVE"),
        "finalized E2 edited to PASS": (dict(mutate_host=lambda h: h["egress_canary"].update(total_connections=3),
                                             mutate_bound=lambda b: by_id(b["evidence"], "E2").update(
                                                 test_status="PASS")), "INVALID"),
        "unrestricted QEMU": (dict(mutate_host=lambda h: h["qemu"].update(cmdline=UNRESTRICTED)), "FAIL"),
        "TCG": (dict(mutate_host=lambda h: h["qemu"].update(
            cmdline=[a.replace("kvm", "tcg") for a in RESTRICTED])), "FAIL"),
        "unsigned image": (dict(mutate_host=lambda h: h["image"].update(
            signature_verified=False, verification_problems=["GnuPG reported BADSIG"])), "FAIL"),
        "image digest mismatch": (dict(mutate_host=lambda h: h["image"].update(
            sha256_actual="9" * 64, signature_verified=False,
            verification_problems=["image SHA-256 differs from the pinned SHA-256"])), "FAIL"),
        **{f"image facts contradict a verification: {name}": (dict(mutate_host=change), "INVALID")
           for name, change in IMAGE_CONTRADICTIONS.items()},
        "signature_verified not a boolean": (dict(mutate_host=lambda h: h["image"].update(
            signature_verified="true")), "INVALID"),
        "signature_verified is 1": (dict(mutate_host=lambda h: h["image"].update(signature_verified=1)), "INVALID"),
        "malformed keyring subkeys, guest failed E3": (dict(
            mutate_guest=lambda g: by_id(g, "E3").update(test_status="FAIL", classification="x"),
            mutate_host=lambda h: h["image"].update(keyring_subkey_fingerprints=[{"x": 1}])), "INVALID"),
        "record image differs": (dict(mutate_guest=lambda g: by_id(g, "A1").update(
            guest_image_sha256="9" * 64)), "INVALID"),
        "other commit in a record": (dict(mutate_guest=lambda g: [r.update(git_commit="b" * 40)
                                                                  for r in g["results"]]), "INVALID"),
        "HEAD moved": (dict(mutate_host=lambda h: h.update(head_at_end="b" * 40)), "INVALID"),
        "dirty at end": (dict(mutate_host=lambda h: h.update(tree_clean_at_end=False)), "INVALID"),
        "run id mismatch": (dict(mutate_host=lambda h: h.update(run_id="another-run-0001")), "INVALID"),
        "providerd hash mismatch": (dict(mutate_host=lambda h: h["build"].update(providerd_sha256="1" * 64)),
                                    "INVALID"),
        "unit hash mismatch": (dict(mutate_host=lambda h: h["build"].update(unit_sha256="1" * 64)), "INVALID"),
        "guest evidence swapped": (dict(mutate_bound=lambda b: b["guest_evidence"]["results"][5].update(
            observed={"edited": True})), "INVALID"),
    }
    for name, (mutation, want) in cases.items():
        got = verdict(**mutation)
        assert got == want, (name, got, want)
    # E3 itself never passes contradicted image facts, before verify-bound looks.
    for name, change in IMAGE_CONTRADICTIONS.items():
        g, h = accepted_run(ev)
        change(h)
        assert by_id(ev.finalize(g, h), "E3")["test_status"] == "FAIL", name
    broken = scratch / "broken.json"
    broken.write_text("{}")
    assert run([sys.executable, "-I", EVIDENCE_TOOL, "verify-bound", broken]).returncode == 1


# --- end to end: guest Run.check -> write -> bind -> verify-bound ------------------

class FakeCompleted:
    def __init__(self, stdout, returncode=0):
        self.stdout, self.stderr, self.returncode = stdout, "", returncode


def guest_run(scratch, name, virt="kvm", route="127.0.0.1", canary_listening=False, fail_ids=(),
              skip_ids=()):
    """Drive the REAL guest code (Run.check, check_environment,
    observe_egress, evaluate_guest_egress, Run.write) with only the two
    system queries it makes (systemd-detect-virt, ip route) faked. The
    "slirp gateway" is 127.0.0.1 and the canary a real local socket:
    listening = what an unrestricted QEMU would forward to; closed = what a
    restricted one leaves unreachable."""
    guest = load(f"guest_e2e_{name}", GUEST / "guest-acceptance.py")
    guest.WORK = scratch / name
    bundle = scratch / f"bundle-{name}"
    bundle.mkdir()
    (bundle / "repo").symlink_to(ROOT)
    canary = socket.socket()
    canary.bind(("127.0.0.1", 0))
    port = canary.getsockname()[1]
    if canary_listening:
        canary.listen(4)
    else:
        canary.close()
    facts = {"run_id": f"e2e-{name}-0001", "git_commit": "c" * 40, "git_tree_clean": True,
             "guest_image": "Arch-Linux-x86_64-cloudimg-x", "guest_image_sha256": "d" * 64,
             "slirp_gateway": "127.0.0.1", "egress_canary_port": port}
    (bundle / "run.json").write_text(json.dumps(facts))
    run_ = guest.Run(bundle)
    run_.facts.update(manifest_files=1, guest_kernel="6.x-arch", systemd_version="systemd 999",
                      providerd_sha256="e" * 64, unit_sha256="f" * 64)

    def fake_sh(*argv, check=True, **_):
        argv = [str(a) for a in argv]
        if argv[:2] == ["systemd-detect-virt", "--vm"]:
            return FakeCompleted(virt + "\n")
        if argv[:5] == ["ip", "-j", "route", "show", "default"]:
            return FakeCompleted(json.dumps([{"gateway": route, "dev": "eth0"}] if route else []))
        raise AssertionError(f"unexpected system call in the offline test: {argv}")
    guest.sh = fake_sh
    guest.check_environment(run_)
    for check in run_.evidence.catalog():
        test_id = check["test_id"]
        if test_id in ("E1", "E2", "E3", "E4") or test_id in skip_ids:
            continue
        run_.check(test_id, "x", lambda t=test_id: (t not in fail_ids, {"simulated": True}))
    guest_exit = run_.write()
    accepted = 0
    if canary_listening:
        canary.settimeout(0.5)
        try:
            while True:
                canary.accept()[0].close()
                accepted += 1
        except OSError:
            pass
        canary.close()
    written = list((scratch / name / "evidence").glob("*.json"))
    assert len(written) == 1, written
    return guest_exit, json.loads(written[0].read_text()), accepted


def host_facts(doc, guest_connections=0, **overrides):
    _, host = accepted_run(load("evidence_hostfacts", EVIDENCE_TOOL))
    host["run_id"] = doc.get("run_id")
    host["egress_canary"]["total_connections"] = 2 + guest_connections
    for key, value in overrides.items():
        if isinstance(value, dict) and isinstance(host.get(key), dict):
            host[key].update(value)
        else:
            host[key] = value
    return host


def check_end_to_end(ev, scratch):
    def status_of(doc, test_id):
        record = next(r for r in doc["results"] if r["test_id"] == test_id)
        return record["test_status"], record["classification"]

    # A valid isolated KVM run: the guest awaits the host, the host passes it.
    guest_exit, doc, _ = guest_run(scratch, "valid")
    assert guest_exit == 0, "a guest ready for host finalization must exit 0"
    for test_id in ("E1", "E2", "E3"):
        assert status_of(doc, test_id) == ("INCONCLUSIVE", "awaiting_host"), (test_id, status_of(doc, test_id))
    assert status_of(doc, "E4")[0] == "PASS" and doc["summary"]["overall"] == "INCONCLUSIVE"
    bound = ev.bind(doc, host_facts(doc))
    assert bound["verdict"] == "PASS", bound["problems"]
    for test_id in ("E1", "E2", "E3"):
        assert status_of(bound["evidence"], test_id)[0] == "PASS"
    path = scratch / "e2e-valid.json"
    path.write_text(json.dumps(bound))
    assert run([sys.executable, "-I", EVIDENCE_TOOL, "verify-bound", path]).returncode == 0
    guest_file = scratch / "e2e-guest.json"
    guest_file.write_text(json.dumps(doc))
    hostfile = scratch / "e2e-host.json"
    hostfile.write_text(json.dumps(host_facts(doc)))
    out = scratch / "e2e-bound-cli.json"
    result = run([sys.executable, "-I", EVIDENCE_TOOL, "bind", "--guest-evidence", guest_file,
                  "--host-facts", hostfile, "--out", out])
    assert result.returncode == 0 and json.loads(out.read_text())["verdict"] == "PASS", result.stdout

    # The same valid guest document with invalid host facts.
    invalid_hosts = {
        "unrestricted QEMU": (dict(qemu={"cmdline": UNRESTRICTED}), "FAIL"),
        "TCG": (dict(qemu={"cmdline": [a.replace("kvm", "tcg") for a in RESTRICTED]}), "FAIL"),
        "unsigned image": (dict(image={"signature_verified": False}), "FAIL"),
        "wrong primary, verification failed": (dict(image={
            "signature_verified": False, "signature_primary_fingerprint": "D" * 40,
            "verification_problems": ["signature's primary key is not the pinned primary"]}), "FAIL"),
        "extra primary, verification failed": (dict(image={
            "signature_verified": False, "keyring_primary_fingerprints": ["A" * 40, "D" * 40],
            "verification_problems": ["keyring must hold exactly one primary key, holds 2"]}), "FAIL"),
        "unapproved subkey claimed verified": (dict(image={"signature_signing_fingerprint": "C" * 40}), "INVALID"),
        "canary dead before": (dict(egress_canary={"live_before": False}), "INCONCLUSIVE"),
        "no QEMU command line": (dict(qemu={"cmdline": []}), "INCONCLUSIVE"),
        "dirty tree at end": (dict(tree_clean_at_end=False), "INVALID"),
        "HEAD moved": (dict(head_at_end="b" * 40), "INVALID"),
        "image is a string": (dict(image="verified"), "INVALID"),
        "qemu is a list": (dict(qemu=["-accel", "kvm"]), "INVALID"),
        "build missing": (dict(build=None), "INVALID"),
    }
    for name, (overrides, want) in invalid_hosts.items():
        got = ev.bind(doc, host_facts(doc, **overrides))["verdict"]
        assert got == want, (name, got, want)
    for host in ({}, None, [], "x"):
        assert ev.bind(doc, host)["verdict"] == "INVALID", host
    # A host that saw the guest at the canary, though the guest saw nothing.
    assert ev.bind(doc, host_facts(doc, guest_connections=1))["verdict"] == "FAIL"

    # Genuine guest failures stay failures whatever the host says.
    genuine = {
        "TCG guest": (dict(virt="qemu"), "E1", ("FAIL", "not_kvm"), "FAIL"),
        "guest reached the host": (dict(canary_listening=True), "E2", ("FAIL", "guest_reached_host_canary"), "FAIL"),
        "no route": (dict(route=None), "E2", ("INCONCLUSIVE", "no_route_to_slirp_gateway"), "INCONCLUSIVE"),
        "confinement violation": (dict(fail_ids={"A10"}), "A10", ("FAIL", None), "FAIL"),
        "unreached checks": (dict(skip_ids={"C16"}), "C16", ("NOT_RUN", None), "NOT_RUN"),
    }
    for name, (kwargs, test_id, want_record, want_verdict) in genuine.items():
        guest_exit, failed, accepted = guest_run(scratch, name.lower().replace(" ", "-"), **kwargs)
        assert guest_exit == 1, name
        assert status_of(failed, test_id) == want_record, (name, status_of(failed, test_id))
        got = ev.bind(failed, host_facts(failed, guest_connections=accepted))["verdict"]
        assert got == want_verdict, (name, got, want_verdict)

    # A malformed run id (uppercase) makes the guest refuse to write evidence.
    _, refused, _ = guest_run(scratch, "Upper-Case")
    assert refused.get("invalid") is True and any("run_id" in p for p in refused["problems"]), refused
    assert ev.bind(refused, host_facts(refused))["verdict"] == "INVALID"

    # Invalid verdicts from a check body never pass and never claim a FAIL.
    guest = load("guest_verdicts", GUEST / "guest-acceptance.py")
    assert guest.verdict_status(True) == "PASS" and guest.verdict_status(False) == "FAIL"
    assert guest.verdict_status("INCONCLUSIVE") == "INCONCLUSIVE"
    for bad in (None, 1, 0, "PASS", "FAIL", "inconclusive", "yes", [], ("INCONCLUSIVE",)):
        assert guest.verdict_status(bad) is None, bad
    _, valid_doc, _ = guest_run(scratch, "verdicts")
    run_ = guest.Run(scratch / "bundle-verdicts")
    for test_id, body in [("A1", lambda: (None, {})), ("A2", lambda: (1, {})), ("A3", lambda: ("PASS", {})),
                          ("A4", lambda: "PASS"), ("A5", lambda: (True,)), ("A6", lambda: (True, {}, 7)),
                          ("A7", lambda: None), ("A8", lambda: (True, {}, "x", "extra"))]:
        assert run_.check(test_id, "x", body) is False, test_id
        assert run_.results[test_id]["test_status"] == "INCONCLUSIVE", (test_id, run_.results[test_id])

    # Malformed or missing evidence never becomes PASS.
    def tampered(mutate):
        broken = json.loads(json.dumps(valid_doc))
        mutate(broken)
        return broken
    malformed_docs = [
        {"invalid": True, "problems": ["x"]}, {}, [], None, "text",
        tampered(lambda d: d.update(results="none")),
        tampered(lambda d: d["results"][0].update(test_id=["E1"])),
        tampered(lambda d: d["results"][0].update(test_status=["PASS"])),
        tampered(lambda d: d["results"][0].update(classification={"x": 1})),
        tampered(lambda d: d["results"].pop()),
        tampered(lambda d: d.update(phase="preparation")),
        tampered(lambda d: d.pop("summary")),
    ]
    for broken in malformed_docs:
        assert ev.bind(broken, host_facts(valid_doc))["verdict"] == "INVALID", str(broken)[:80]
        if isinstance(broken, dict):
            assert ev.validate(broken, "execution"), "malformed evidence validated"
    for text in ["", "{", "[]", "{}", '{"verdict": "PASS"}', json.dumps({"kind": "remote-provider-kvm-acceptance",
                                                                     "verdict": "PASS", "problems": []})]:
        path = scratch / "malformed-bound.json"
        path.write_text(text)
        result = run([sys.executable, "-I", EVIDENCE_TOOL, "verify-bound", path])
        assert result.returncode == 1 and '"verdict": "PASS"' not in result.stdout, (text, result.stdout)
    result = run([sys.executable, "-I", EVIDENCE_TOOL, "verify-bound", scratch / "absent.json"])
    assert result.returncode == 1 and '"INVALID"' in result.stdout
    result = run([sys.executable, "-I", EVIDENCE_TOOL, "validate", scratch / "absent.json"])
    assert result.returncode == 1


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
    assert runner.results["A4"]["test_status"] == "INCONCLUSIVE"
    assert runner.results["A4"]["classification"] == "invalid_verdict"
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
        evidence_module = load("evidence_chain", EVIDENCE_TOOL)
        guest_module = load("guest_e2", GUEST / "guest-acceptance.py")
        check_e2(evidence_module, guest_module)
        check_binding(evidence_module, scratch)
        check_end_to_end(evidence_module, scratch)
        assert shims.calls() == [], shims.calls()
    print("remote provider KVM harness, offline: preparation modes run nothing forbidden and report NOT_RUN; "
          "--execute refuses on every missing gate; KVM-only, restrict=on, no workflow executes it; "
          "network plan matches the DNS classifier and probe targets; evidence rules reject fabricated "
          "PASS, gaps and leaks; mock scenarios, DNS and rebinding; probe classification; guest runner "
          "fail-closed. (Harness only: real confinement remains NOT_RUN.)")


if __name__ == "__main__":
    main()

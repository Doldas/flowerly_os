#!/usr/bin/env python3
"""Remote provider Arch/KVM acceptance, guest side (#263). TEST ONLY.

    guest-acceptance.py <bundle-dir>

Runs as root INSIDE the disposable guest that
scripts/remote-provider-arch-kvm-acceptance.sh booted for one run, and
refuses to run anywhere else: it needs the run's marker file (written by
cloud-init from the host harness), a KVM virtual machine, the hostname the
harness chose and a bundle whose manifest matches. It changes accounts,
groups, units, DNS and the trust store of THAT GUEST ONLY.

It installs the provider with the real operator procedure, runs the
confinement probe inside the real unit, real providerd against the guest
mock, the socket-identity matrix and the lifecycle cases, and writes one
evidence document (scripts/remote-provider-kvm-evidence.py rules):
- a check reports PASS only when its assertion held;
- a harness error makes it INCONCLUSIVE, never PASS;
- a check that was not reached is NOT_RUN.

Nothing here contacts a real server or uses a real key: the credential is
a random dummy encrypted with this guest's systemd-creds, and every
destination is a guest-local mock address (testbed/remote-provider-kvm/
network.json) on a guest that has no route out.
"""
import grp
import hashlib
import importlib.util
import json
import os
import pathlib
import pwd
import re
import secrets
import shutil
import socket
import subprocess
import sys
import time
import traceback

MARKER = pathlib.Path("/etc/flowerly-kvm-disposable-guest")
WORK = pathlib.Path("/var/lib/flowerly-kvm")
INSTANCE = "probe"
MODEL = "gpt-oss:20b"
PROVIDER_USER = "flowerly-provider"
CLIENT_GROUP = "flowerly-provider-clients"
SERVICE_USER = "flowerly"
OTHER_USER = "flowerly-kvm-other"
PLAIN_USER = "flowerly-kvm-user"
EXTRA_GROUP = "flowerly-kvm-extra"
MOCK_USER = "flowerly-kvm-mock"
UNIT = f"flowerly-provider-remote@{INSTANCE}.service"
SOCKET = f"/run/flowerly-provider/{INSTANCE}.sock"
RUNTIME_DROPINS = pathlib.Path(f"/run/systemd/system/{UNIT}.d")
LIBEXEC = pathlib.Path("/usr/local/libexec/flowerly")
EVIDENCE_DIR = pathlib.Path("/var/log/flowerly-provider-acceptance")
PROBE = pathlib.Path("/usr/local/share/flowerly-kvm/provider-unit-probe.py")
SNAPSHOT_PATHS = ["/etc/systemd/system", "/etc/flowerly", "/etc/sysusers.d", "/usr/local/libexec/flowerly",
                  "/var/lib/flowerly-provider-operator", "/usr/lib/sysusers.d"]


class Harness(Exception):
    """A harness problem: the check is INCONCLUSIVE, never PASS."""


INCONCLUSIVE = "INCONCLUSIVE"


def verdict_status(verdict):
    """A check body's verdict -> evidence status, explicitly:
    True -> PASS, False -> FAIL, the string INCONCLUSIVE -> INCONCLUSIVE.
    Anything else (None, 1, "PASS", "yes", ...) is a harness defect and
    becomes INCONCLUSIVE: never PASS, and never a FAIL that would claim an
    observation the check did not make."""
    if verdict is True:
        return "PASS"
    if verdict is False:
        return "FAIL"
    if verdict == INCONCLUSIVE and isinstance(verdict, str):
        return "INCONCLUSIVE"
    return None


def sh(*argv, check=True, env=None, input_text=None, timeout=300):
    result = subprocess.run([str(a) for a in argv], capture_output=True, text=True, env=env,
                            input=input_text, timeout=timeout)
    if check and result.returncode != 0:
        raise Harness(f"{argv[0]} {argv[1] if len(argv) > 1 else ''} exited {result.returncode}: "
                      f"{result.stderr.strip()[-300:]}")
    return result


def sha256(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


def dummy_credential(name):
    """Encrypt a fresh random dummy key for `name` with this guest's
    systemd-creds; the plaintext exists only in this call and the pipe."""
    pathlib.Path("/etc/credstore.encrypted").mkdir(mode=0o700, exist_ok=True)
    dummy = f"flowerly-kvm-dummy-key-{secrets.token_hex(24)}"
    sh("systemd-creds", "encrypt", f"--name={name}.key", "-", f"/etc/credstore.encrypted/{name}.key",
       input_text=dummy)
    return hashlib.sha256(dummy.encode()).hexdigest()


def utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Run:
    def __init__(self, bundle):
        self.bundle = pathlib.Path(bundle)
        self.repo = self.bundle / "repo"
        self.facts = json.loads((self.bundle / "run.json").read_text())
        self.evidence = load("evidence", self.repo / "scripts/remote-provider-kvm-evidence.py")
        self.catalog = {check["test_id"]: check for check in self.evidence.catalog()}
        self.network = json.loads((self.repo / "testbed/remote-provider-kvm/network.json").read_text())
        self.results = {}
        self.state = WORK / "mock-state"
        self.client = self.repo / "testbed/remote-provider-kvm/guest/provider-client.py"
        self.operator = self.repo / "scripts/remote-provider-operator.sh"
        self.generation = 0

    # --- records -------------------------------------------------------

    def check(self, test_id, expected, body):
        """Run `body` -> (verdict, observed[, classification]); see
        `verdict_status`. E1-E3 return INCONCLUSIVE ('awaiting_host') until
        the host finalizes them."""
        started = utc()
        try:
            outcome = body()
            if not isinstance(outcome, tuple) or len(outcome) not in (2, 3):
                raise Harness(f"check body returned {type(outcome).__name__}, not (verdict, observed[, class])")
            verdict, observed = outcome[0], outcome[1]
            classification = outcome[2] if len(outcome) == 3 else None
            status = verdict_status(verdict)
            if status is None:
                status, classification = "INCONCLUSIVE", "invalid_verdict"
                observed = {"invalid_verdict": repr(verdict)[:80], "observed": observed}
            if classification is not None and not isinstance(classification, str):
                raise Harness("classification must be a string")
        except Harness as err:
            status, observed, classification = "INCONCLUSIVE", {"harness_error": str(err)[:400]}, "harness_error"
        except Exception as err:  # noqa: BLE001 - any surprise is inconclusive, never a pass
            status, classification = "INCONCLUSIVE", "harness_exception"
            observed = {"harness_exception": f"{type(err).__name__}: {str(err)[:300]}",
                        "where": traceback.format_exc().splitlines()[-3:]}
        self.results[test_id] = self.evidence.record(
            self.catalog[test_id], self.facts, status, expected=expected, observed=observed,
            classification=classification, started=started, finished=utc())
        print(f"{test_id}: {status}", flush=True)
        return status == "PASS"

    def not_run(self, test_id, reason):
        if test_id not in self.results:
            self.results[test_id] = self.evidence.record(self.catalog[test_id], self.facts, "NOT_RUN",
                                                         skip_reason=reason)

    def write(self):
        for test_id in self.catalog:
            self.not_run(test_id, "not reached in this run")
        ordered = [self.results[test_id] for test_id in self.catalog]
        doc = self.evidence.document("execution", self.facts["run_id"], ordered)
        problems = self.evidence.validate(doc, "execution")
        out = WORK / "evidence"
        out.mkdir(mode=0o755, parents=True, exist_ok=True)
        path = out / f"remote-provider-kvm-{self.facts['run_id']}.json"
        if problems:
            # Never ship a document the validator rejects; say why instead.
            path = out / f"remote-provider-kvm-{self.facts['run_id']}.INVALID.json"
            doc = {"invalid": True, "problems": problems}
        path.write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
        ready = not problems and ready_for_host(doc)
        print(json.dumps({"evidence": str(path), "valid": not problems,
                          "overall": None if problems else doc["summary"]["overall"],
                          "ready_for_host_finalization": ready}))
        # The guest document can never be PASS by itself (E1-E3 await the
        # host); exit 0 means "every guest-decided check passed and E1-E3
        # await the host", which is the only state the host can finalize
        # into an acceptance candidate.
        return 0 if ready else 1

    # --- guest plumbing ------------------------------------------------

    def scenario(self, word, *extra):
        self.generation += 1
        (self.state / "scenario").write_text(" ".join([word, str(self.generation), *extra]) + "\n")

    def events(self):
        path = self.state / "events.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]

    def mark(self):
        return len(self.events())

    def since(self, mark, **match):
        return [e for e in self.events()[mark:] if all(e.get(k) == v for k, v in match.items())]

    def as_user(self, user, *argv, groups=None, check=False):
        """Run as `user` with exactly `groups` (names) when given, else the
        account's /etc/group memberships (runuser)."""
        if groups is None:
            return sh("runuser", "-u", user, "--", *argv, check=check)
        entry = pwd.getpwnam(user)
        gids = ",".join(str(grp.getgrnam(g).gr_gid) for g in groups) if groups else ""
        group_args = [f"--groups={gids}"] if gids else ["--clear-groups"]
        return sh("setpriv", f"--reuid={entry.pw_uid}", f"--regid={entry.pw_gid}", *group_args, "--",
                  *argv, check=check)

    def client_json(self, user, *args, groups=None):
        result = self.as_user(user, "python3", "-I", self.client, *args, groups=groups)
        try:
            return json.loads(result.stdout)
        except ValueError:
            raise Harness(f"client gave no JSON (exit {result.returncode}): {result.stderr[-300:]}")

    def submit(self, tag, **kw):
        args = ["submit", SOCKET, MODEL, tag]
        if kw.get("request_id"):
            args += ["--request-id", kw["request_id"]]
        if kw.get("variant"):
            args += ["--variant", str(kw["variant"])]
        if kw.get("no_wait"):
            args.append("--no-wait")
        return self.client_json(kw.get("user", PROVIDER_USER), *args, groups=kw.get("groups"))

    def restart_provider(self):
        self.stop_provider()
        # RuntimeDirectoryPreserve=yes keeps the old socket file: remove it
        # so the wait below sees the new instance's socket, not the old one.
        pathlib.Path(SOCKET).unlink(missing_ok=True)
        sh("systemctl", "start", UNIT)
        for _ in range(100):
            if os.path.exists(SOCKET):
                return
            time.sleep(0.1)
        raise Harness("provider socket did not appear")

    def stop_provider(self):
        sh("systemctl", "stop", UNIT, check=False)
        sh("systemctl", "reset-failed", UNIT, check=False)

    def runtime_dropin(self, name, text):
        RUNTIME_DROPINS.mkdir(parents=True, exist_ok=True)
        (RUNTIME_DROPINS / name).write_text(text)
        sh("systemctl", "daemon-reload")

    def remove_runtime_dropin(self, name):
        (RUNTIME_DROPINS / name).unlink(missing_ok=True)
        sh("systemctl", "daemon-reload")

    def operator_cmd(self, *args, env_extra=None, check=False):
        env = dict(os.environ, FLOWERLY_OPERATOR_PROVIDERD=str(self.bundle / "bin/providerd"))
        env.update(env_extra or {})
        return sh("bash", self.operator, *args, env=env, check=check, timeout=600)

    def journal(self, unit, since):
        return sh("journalctl", "--no-pager", "-o", "cat", "-u", unit, "--since", since, check=False).stdout

    def snapshot(self):
        files = {}
        for root in SNAPSHOT_PATHS:
            for path in sorted(pathlib.Path(root).rglob("*")) if os.path.isdir(root) else []:
                if path.is_symlink():
                    files[str(path)] = "symlink:" + os.readlink(path)
                elif path.is_file():
                    files[str(path)] = sha256(path)
        accounts = sorted(line.split(":")[0] + ":" + line.split(":")[2]
                          for line in open("/etc/passwd") if line.strip())
        groups = sorted(":".join(line.strip().split(":")[i] for i in (0, 2, 3))
                        for line in open("/etc/group") if line.strip())
        return {"files": files, "accounts": accounts, "groups": groups}


# --- guard and environment ------------------------------------------------

def early_guard():
    """Before reading anything: root, a disposable-guest marker, KVM."""
    if os.geteuid() != 0:
        sys.exit("guest-acceptance: run as root inside the disposable guest")
    if not MARKER.is_file():
        sys.exit("guest-acceptance: no disposable-guest marker; refusing")
    virt = subprocess.run(["systemd-detect-virt", "--vm"], capture_output=True, text=True).stdout.strip()
    if virt != "kvm":
        sys.exit(f"guest-acceptance: not a KVM guest ({virt or 'none'}); refusing")


def guard(run):
    """Refuse anywhere but this run's disposable KVM guest."""
    run_id = run.facts["run_id"]
    if MARKER.read_text().strip() != run_id:
        sys.exit("guest-acceptance: the disposable-guest marker is for another run; refusing")
    if socket.gethostname() != f"flowerly-kvm-{run_id[:20]}":
        sys.exit("guest-acceptance: unexpected hostname; refusing")
    manifest = json.loads((run.bundle / "manifest.json").read_text())
    for rel, digest in manifest.items():
        if sha256(run.bundle / rel) != digest:
            sys.exit(f"guest-acceptance: bundle file {rel} does not match the manifest; refusing")
    run.facts["manifest_files"] = len(manifest)


def collect_facts(run):
    run.facts.update(
        guest_kernel=os.uname().release,
        systemd_version=sh("systemctl", "--version").stdout.splitlines()[0],
        providerd_sha256=sha256(run.bundle / "bin/providerd"),
        unit_sha256=sha256(run.repo / "distro/templates/remote-inference/flowerly-provider-remote@.service"))


AWAITING_HOST = "awaiting_host"
HOST_CHECKS = ("E1", "E2", "E3")


def ready_for_host(doc):
    """Every guest-decided check PASS; E1-E3 INCONCLUSIVE awaiting the host."""
    for record in doc["results"]:
        if record["test_id"] in HOST_CHECKS:
            if (record["test_status"], record["classification"]) != ("INCONCLUSIVE", AWAITING_HOST):
                return False
        elif record["test_status"] != "PASS":
            return False
    return True


def evaluate_guest_egress(obs):
    """The guest's half of E2 -> (status, classification). Never PASS: an
    unanswered connection proves nothing by itself. PASS needs the host's
    half too (scripts/remote-provider-kvm-evidence.py finalize): QEMU's
    running command line has restrict=on and the host canary the guest
    aimed at stayed untouched while it was proven live.

    obs keys: local_control ('connected' proves this probing method sees a
    working path), default_route_via (the gateway the kernel would send the
    attempt to), gateway (QEMU's slirp gateway, mapped to the host's
    loopback when NOT restricted), canary_attempt ('connected' or an errno
    name; None if not attempted)."""
    if obs.get("canary_attempt") == "connected":
        return "FAIL", "guest_reached_host_canary"
    if obs.get("local_control") != "connected":
        return "INCONCLUSIVE", "probe_method_unproven"
    if not obs.get("gateway") or obs.get("default_route_via") != obs.get("gateway"):
        # Without a route the attempt never reaches QEMU: its failure says
        # nothing about restrict=on.
        return "INCONCLUSIVE", "no_route_to_slirp_gateway"
    if obs.get("canary_attempt") is None:
        return "INCONCLUSIVE", "canary_not_attempted"
    return INCONCLUSIVE, AWAITING_HOST


def observe_egress(run):
    """Collect the guest's E2 observations, before any route change."""
    obs = {"gateway": run.facts.get("slirp_gateway"), "canary_port": run.facts.get("egress_canary_port")}
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        try:
            with socket.create_connection(listener.getsockname(), timeout=5):
                obs["local_control"] = "connected"
        except OSError as err:
            obs["local_control"] = type(err).__name__
    routes = json.loads(sh("ip", "-j", "route", "show", "default", check=False).stdout or "[]")
    obs["default_routes"] = [{k: r.get(k) for k in ("gateway", "dev")} for r in routes]
    obs["default_route_via"] = routes[0].get("gateway") if len(routes) == 1 else None
    obs["canary_attempt"] = None
    if obs["gateway"] and isinstance(obs["canary_port"], int):
        try:
            with socket.create_connection((obs["gateway"], obs["canary_port"]), timeout=10) as conn:
                conn.sendall(f"guest-egress-canary {run.facts['run_id']}\n".encode())
                obs["canary_attempt"] = "connected"
        except OSError as err:
            obs["canary_attempt"] = errname(err)
    return obs


def errname(err):
    import errno
    return errno.errorcode.get(err.errno, type(err).__name__) if err.errno else type(err).__name__


def check_environment(run):
    """E1-E4, guest halves. E1-E3 stay INCONCLUSIVE ('awaiting_host') until
    the host adds what only it can see; E4 is fully decided here."""
    virt = sh("systemd-detect-virt", "--vm", check=False).stdout.strip()
    run.check("E4", "marker, KVM, hostname and bundle manifest match this run", lambda: (True, {
        "marker_run_id": run.facts["run_id"], "virt": virt, "hostname": socket.gethostname(),
        "manifest_files_verified": run.facts["manifest_files"]}))
    # `kvm` here and `-accel kvm` on QEMU's running command line (host half).
    run.check("E1", "guest sees KVM; host verifies QEMU's -accel kvm",
              lambda: (INCONCLUSIVE if virt == "kvm" else False, {"guest_virt": virt},
                       AWAITING_HOST if virt == "kvm" else "not_kvm"))
    run.check("E3", "host verified the pinned image digest and signature before boot",
              lambda: (INCONCLUSIVE, {"guest": "cannot verify its own disk image"}, AWAITING_HOST))

    def e2():
        obs = observe_egress(run)
        status, classification = evaluate_guest_egress(obs)
        return (False if status == "FAIL" else INCONCLUSIVE), obs, classification
    run.check("E2", "guest reaches no host path through QEMU (host canary untouched, restrict=on verified)", e2)


def setup_network(run):
    net = run.network
    iface = net["interface"]
    sh("ip", "link", "add", iface, "type", "dummy")
    for role, address in net["addresses"].items():
        if role.startswith("loopback"):
            continue
        prefix = "128" if ":" in address else "32"
        sh("ip", "addr", "add", f"{address}/{prefix}", "dev", iface, *(["nodad"] if ":" in address else []))
    sh("ip", "link", "set", iface, "up")
    # Deterministic resolution: only the mock DNS stub, no cache, no resolved.
    sh("systemctl", "stop", "systemd-resolved.service", check=False)
    sh("systemctl", "mask", "systemd-resolved.service", check=False)
    resolv = pathlib.Path("/etc/resolv.conf")
    if resolv.is_symlink() or resolv.exists():
        resolv.unlink()
    resolv.write_text(f"nameserver {net['dns_stub']}\noptions attempts:1 timeout:2\n")
    nss = pathlib.Path("/etc/nsswitch.conf")
    nss.write_text(re.sub(r"(?m)^hosts:.*$", "hosts: files dns", nss.read_text()))
    hosts = pathlib.Path("/etc/hosts").read_text()
    if net["domain"] in hosts:
        raise Harness("/etc/hosts already names the test domain")
    # Defence in depth after E2 (which needed the route): no default route,
    # and every mock address is delivered locally, never forwarded.
    for family in ("-4", "-6"):
        for _ in range(8):
            if not json.loads(sh("ip", "-j", family, "route", "show", "default").stdout or "[]"):
                break
            sh("ip", family, "route", "del", "default")
        else:
            raise Harness(f"could not remove the {family} default route")
    for role, address in net["addresses"].items():
        if role.startswith("loopback"):
            continue
        route = sh("ip", "route", "get", address, check=False).stdout
        if not route.startswith("local "):
            raise Harness(f"{role} {address} is not a local address in this guest: {route.strip()[:120]}")


def setup_pki_and_mock(run):
    pki = WORK / "pki"
    pki.mkdir(mode=0o700, parents=True)
    names = run.network["names"]
    sh("bash", run.repo / "testbed/remote-provider-kvm/guest/make-test-pki.sh", pki,
       names["mock"], names["redirect_target"], names["wrong_name"])
    # Trust the run's test CA in THIS guest only.
    sh("trust", "anchor", "--store", pki / "test-ca.crt")
    sh("update-ca-trust", "extract", check=False)
    sh("useradd", "--system", "--no-create-home", "--shell", "/usr/bin/nologin", MOCK_USER)
    for key in pki.glob("*.key"):
        shutil.chown(key, MOCK_USER)
    shutil.chown(pki, MOCK_USER)
    run.state.mkdir(mode=0o755, parents=True)
    shutil.chown(run.state, MOCK_USER)
    key_sha = dummy_credential(INSTANCE)
    mock = run.repo / "testbed/remote-provider-kvm/guest/mock-provider.py"
    sh("systemd-run", "--unit=flowerly-kvm-mock", f"--uid={MOCK_USER}", "-p",
       "AmbientCapabilities=CAP_NET_BIND_SERVICE", "-p", "NoNewPrivileges=yes",
       "python3", "-I", mock, "--network", run.repo / "testbed/remote-provider-kvm/network.json",
       "--pki", pki, "--state", run.state, "--model", MODEL, "--key-sha256", key_sha)
    for _ in range(100):
        if (run.state / "ports.json").exists():
            return
        time.sleep(0.1)
    raise Harness("mock did not start")


def setup_fixtures(run):
    """Real state to deny and decoys only the unit can make unreadable."""
    sh("install", "-D", "-m", "0755", run.bundle / "bin/flowerlyd", "/usr/bin/flowerlyd")
    overlay = run.repo / "distro/overlay"
    sh("install", "-D", "-m", "0644", overlay / "usr/lib/systemd/system/flowerlyd.service",
       "/usr/lib/systemd/system/flowerlyd.service")
    sh("install", "-D", "-m", "0644", overlay / "usr/lib/sysusers.d/flowerly.conf",
       "/usr/lib/sysusers.d/flowerly.conf")
    sh("systemd-sysusers", "/usr/lib/sysusers.d/flowerly.conf")
    sh("systemctl", "daemon-reload")
    sh("systemctl", "start", "flowerlyd.service")
    time.sleep(2)
    for path in ["/var/lib/flowerly/kvm-decoy-world-readable", "/run/flowerly/kvm-decoy-world-readable"]:
        pathlib.Path(path).write_text("decoy\n")
        os.chmod(path, 0o644)
    os.makedirs("/var/lib/flowerly-evolution", exist_ok=True)
    pathlib.Path("/etc/credstore").mkdir(mode=0o700, exist_ok=True)
    pathlib.Path("/etc/credstore/other.cred").write_text("unrelated\n")
    sh("systemd-creds", "encrypt", "--name=other.key", "-", "/etc/credstore.encrypted/other.key",
       input_text="unrelated-test-value")
    os.makedirs("/root/.ssh", mode=0o700, exist_ok=True)
    if not os.path.exists("/root/.ssh/id_ed25519"):
        sh("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", "/root/.ssh/id_ed25519")
    sh("useradd", "--create-home", "--shell", "/usr/bin/nologin", OTHER_USER)
    home = pathlib.Path(f"/home/{OTHER_USER}")
    os.chmod(home, 0o755)
    (home / "world-readable.txt").write_text("other user's data\n")
    os.chmod(home / "world-readable.txt", 0o644)
    sh("useradd", "--no-create-home", "--shell", "/usr/bin/nologin", PLAIN_USER)
    sh("groupadd", EXTRA_GROUP)


# --- Part C, first half: install ---------------------------------------------

def install_provider(run):
    api_base = f"https://{run.network['names']['mock']}:{run.network['mock_port']}"

    def c1():
        result = run.operator_cmd("install", INSTANCE, api_base, "open-webui", MODEL)
        active = sh("systemctl", "is-active", UNIT, check=False).stdout.strip()
        enabled = sh("systemctl", "is-enabled", UNIT, check=False).stdout.strip()
        passed = result.returncode == 0 and active != "active" and enabled not in ("enabled", "enabled-runtime")
        return passed, {"install_exit": result.returncode, "is_active": active, "is_enabled": enabled}
    if not run.check("C1", "install exits 0; unit neither active nor enabled", c1):
        raise Harness("install failed; later checks would test nothing")
    # Outside the operator's paths, so it never affects rollback or C16.
    sh("install", "-D", "-m", "0755", run.repo / "scripts/provider-unit-probe.py", PROBE)
    sh("install", "-D", "-m", "0644", run.repo / "testbed/remote-provider-kvm/probe-targets.json",
       "/usr/local/share/flowerly-kvm/probe-targets.json")
    run.facts["providerd_installed_sha256"] = sha256(LIBEXEC / "providerd")

    def c2():
        source = pathlib.Path(f"/etc/credstore.encrypted/{INSTANCE}.key")
        aside = WORK / "credential-aside"
        mark = run.mark()
        shutil.move(source, aside)
        try:
            started = sh("systemctl", "start", UNIT, check=False)
            time.sleep(1)
            state = sh("systemctl", "is-active", UNIT, check=False).stdout.strip()
        finally:
            run.stop_provider()
            shutil.move(aside, source)
        connections = run.since(mark, kind="connect")
        return (started.returncode != 0 and state != "active" and not connections,
                {"start_exit": started.returncode, "state": state, "mock_connections": len(connections)})
    run.check("C2", "unit fails to start without its credential; no connection", c2)


# --- Part A ------------------------------------------------------------------

def part_a(run):
    # o+x on flowerlyd's 0700 directories for the probe only, so the
    # world-readable decoys inside are open to ordinary permissions and a
    # denial can only come from the unit. Restored right after.
    modes = {path: os.stat(path).st_mode & 0o7777 for path in ("/var/lib/flowerly", "/run/flowerly")}
    for path, mode in modes.items():
        os.chmod(path, mode | 0o001)
    try:
        probe_in_unit(run)
    finally:
        for path, mode in modes.items():
            os.chmod(path, mode)


def probe_in_unit(run):
    control_path = WORK / "probe-control.json"
    targets = "/usr/local/share/flowerly-kvm/probe-targets.json"
    control = sh("python3", "-I", PROBE, "--targets", targets,
                 "--control-out", control_path, check=False)
    if control.returncode != 0:
        raise Harness(f"control run could not reach every target: {control.stdout[-300:]}")
    sh("install", "-m", "0644", control_path, "/usr/local/share/flowerly-kvm/probe-control.json")
    output = WORK / "probe-unit.json"
    run.runtime_dropin("90-probe.conf", (
        "[Service]\nType=oneshot\nRestart=no\nRemainAfterExit=no\nExecStart=\n"
        f"ExecStart=/usr/bin/python3 -I {PROBE} --targets {targets} "
        "--control /usr/local/share/flowerly-kvm/probe-control.json\n"
        f"Environment=FLOWERLY_PROBE_INSTANCE={INSTANCE}\n"
        f"StandardOutput=truncate:{output}\n"))
    try:
        sh("systemctl", "start", UNIT, check=False, timeout=180)
    finally:
        run.stop_provider()
        run.remove_runtime_dropin("90-probe.conf")
    try:
        report = json.loads(output.read_text())
    except (OSError, ValueError):
        report = None
    checks = (report or {}).get("checks", {})
    exposure = sh("systemd-analyze", "security", "--no-pager", UNIT, check=False).stdout.splitlines()[-1:]

    def group(test_id, expected, selector, extra=None):
        def body():
            if report is None:
                raise Harness("the probe produced no report inside the unit")
            if not report.get("controlled"):
                raise Harness("the in-unit probe ran without the control run")
            chosen = {name: entry for name, entry in checks.items() if selector(name)}
            if not chosen:
                raise Harness("no probe checks matched")
            inconclusive = [n for n, e in chosen.items() if e["required"] and e["classification"] == "inconclusive"]
            if inconclusive:
                raise Harness(f"inconclusive: {inconclusive}")
            passed = all(e["ok"] for e in chosen.values() if e["required"])
            if extra:
                passed = passed and extra(chosen)
            return passed, {name: {k: e.get(k) for k in ("observed", "classification", "control", "dac_open")}
                            for name, e in chosen.items()}
        run.check(test_id, expected, body)

    group("A1", "uid/gid match the provider account on all four fields; no supplementary group",
          lambda n: n.startswith(("uid ", "gid ", "not root", "no supplementary")))
    group("A2", "all capability sets zero", lambda n: n.startswith("status Cap"))
    group("A3", "NoNewPrivs 1, Seccomp 2", lambda n: n in ("status NoNewPrivs", "status Seccomp"))
    group("A4", "every unshare refused by the OS", lambda n: n.startswith("unshare "))

    def proves_sandbox(chosen):
        # At least one denied path was readable by ordinary permissions.
        return any(e.get("dac_open") for e in chosen.values())
    group("A5", "Flowerly state denied, including a world-readable decoy",
          lambda n: n.startswith(("read /var/lib/flowerly", "read /run/flowerly")) and "operator" not in n,
          proves_sandbox)
    group("A6", "unrelated secrets, homes, operator ledger and evidence denied, including a world-readable decoy",
          lambda n: n.startswith(("read /etc/credstore", "read /etc/shadow", "read /etc/gshadow", "read /root",
                                  "read /home", "read /var/lib/flowerly-provider-operator",
                                  "read /var/log/flowerly-provider-acceptance")), proves_sandbox)
    group("A7", "exactly <instance>.key exposed, non-empty, contents unread",
          lambda n: n.startswith(("credentials directory", "exposed credential")))
    group("A8", "no write outside the runtime directory", lambda n: n.startswith("write /"))
    group("A9", "setuid, /proc/1, /proc/sys, /dev/kvm, /dev/mem refused",
          lambda n: n.startswith(("setuid", "read /proc", "list /proc", "write /proc", "open /dev")))
    group("A10", "loopback v4/v6 blocked_by_os with a live listener", lambda n: n.startswith("connect loopback"))
    group("A11", "link-local and metadata blocked_by_os with a live listener",
          lambda n: n.startswith(("connect link_local", "connect metadata")))
    group("A12", "forbidden socket families refused", lambda n: n.endswith(" socket"))
    group("A13", "loopback DNS stub reachable", lambda n: n.startswith("connect DNS stub"))

    def a14():
        if report is None:
            raise Harness("the probe produced no report inside the unit")
        chosen = {n: e for n, e in checks.items() if "(application policy only)" in n}
        if any(e.get("control") != "allowed" for e in chosen.values()):
            raise Harness("a private/public sink was not reachable in the control run")
        observed = {n: e["classification"] for n, e in chosen.items()}
        # The plan documents that the unit does NOT block these: a mismatch
        # makes the documentation wrong and must be reviewed.
        return all(c == "allowed" for c in observed.values()), observed, "documents_application_only_policy"
    run.check("A14", "private, CGNAT, ULA and public sinks reachable from the unit (not an OS boundary)", a14)
    run.facts["systemd_analyze_exposure"] = exposure

    def a15():
        dummy_credential("second")
        sh("gpasswd", "-a", PROVIDER_USER, EXTRA_GROUP)
        since = utc().replace("T", " ").rstrip("Z")
        try:
            started = sh("systemctl", "start", UNIT, check=False)
            time.sleep(2)
            journal = run.journal(UNIT, since)
            state = sh("systemctl", "is-active", UNIT, check=False).stdout.strip()
            run.stop_provider()
            second = run.operator_cmd("install", "second", f"https://{run.network['names']['mock']}:8443",
                                      "open-webui", MODEL)
        finally:
            sh("gpasswd", "-d", PROVIDER_USER, EXTRA_GROUP, check=False)
            pathlib.Path("/etc/credstore.encrypted/second.key").unlink(missing_ok=True)
        refused_unit = state != "active" and "supplementary group" in journal
        refused_install = second.returncode != 0 and "must hold no other group" in second.stderr
        return refused_unit and refused_install, {
            "start_exit": started.returncode, "state": state, "journal_names_supplementary_group":
            "supplementary group" in journal, "second_install_exit": second.returncode,
            "second_install_refused_for_group": "must hold no other group" in second.stderr}
    run.check("A15", "extra group: real unit fails at startup, install refuses", a15)


# --- Part B ------------------------------------------------------------------

def expect_failure(result, cls, outcome):
    final = result.get("final") or {}
    error = final.get("error") or (result.get("submit") or {}).get("error") or {}
    return error.get("class") == cls and error.get("outcome") == outcome, error


def part_b(run):
    names = run.network["names"]

    def b1():
        run.scenario("ok")
        mark = run.mark()
        preflight = run.operator_cmd("preflight", INSTANCE, str(EVIDENCE_DIR))
        http = run.since(mark, kind="http")
        chats = [e for e in http if "chat" in e["path"]]
        passed = (preflight.returncode == 0 and any("models" in e["path"] and e["status"] == 200 and
                                                    e["auth"] == "match" for e in http) and not chats)
        return passed, {"preflight_exit": preflight.returncode, "requests": [(e["path"], e["status"]) for e in http]}
    run.check("B1", "discovery over verified TLS to 192.88.99.10 with default policy; no chat", b1)

    def b2():
        run.scenario("ok")
        mark = run.mark()
        accepted = run.operator_cmd("accept", INSTANCE, f"kvm-acceptance-{run.facts['run_id']}", str(EVIDENCE_DIR))
        chats = [e for e in run.since(mark, kind="http") if "chat" in e["path"]]
        files = sorted(EVIDENCE_DIR.glob("*-accept.json"))
        leaks = run.evidence.leaks(files[-1].read_text()) if files else ["no_evidence_file"]
        passed = accepted.returncode == 0 and len(chats) == 1 and not leaks and not chats[0]["tools_offered"]
        return passed, {"accept_exit": accepted.returncode, "chat_requests": len(chats), "evidence_leaks": leaks}
    run.check("B2", "exactly one chat request; evidence clean", b2)

    def refused_address(scenario, role):
        def body():
            run.scenario(scenario)
            run.restart_provider()
            mark = run.mark()
            result = run.submit(scenario)
            ok, error = expect_failure(result, "address_refused", "not_sent")
            sink = run.since(mark, kind="connect", role=role)
            mock = run.since(mark, kind="connect", role="mock")
            return ok and not sink and not mock, {"error": error, "sink_connections": len(sink),
                                                  "mock_connections": len(mock)}
        return body
    for test_id, scenario, role in [("B3", "addr-private", "private_rfc1918"), ("B4", "addr-cgnat", "cgnat"),
                                    ("B5", "addr-loopback", "loopback_v4"),
                                    ("B6", "addr-link-local", "link_local_v4"),
                                    ("B7", "addr-metadata", "metadata_v4"), ("B8", "addr-ula", "ula")]:
        run.check(test_id, f"address_refused/not_sent; {role} sink untouched", refused_address(scenario, role))

    def b9():
        run.scenario("rebind", "1")
        run.restart_provider()
        mark = run.mark()
        discovered = run.client_json(PROVIDER_USER, "discover", SOCKET, MODEL)
        result = run.submit("rebind")
        ok, error = expect_failure(result, "address_refused", "not_sent")
        answers = [e["answer_role"] for e in run.since(mark, kind="dns", qtype=1)
                   if e["name"] == names["mock"]]
        sink = run.since(mark, kind="connect", role="private_rfc1918")
        passed = (discovered.get("ok") and ok and not sink and answers[:1] == ["public_mock"]
                  and "private_rfc1918" in answers)
        return passed, {"discover_ok": discovered.get("ok"), "error": error, "a_answers": answers,
                        "private_sink_connections": len(sink)}
    run.check("B9", "first answer public, later private: chat refused at connection time", b9)

    def failure_case(scenario, cls, outcome, extra=None):
        def body():
            run.scenario(scenario)
            run.restart_provider()
            mark = run.mark()
            result = run.submit(scenario)
            ok, error = expect_failure(result, cls, outcome)
            observed = {"error": error}
            if extra:
                more_ok, more = extra(mark, result)
                ok = ok and more_ok
                observed.update(more)
            return ok, observed
        return body

    def no_http(mark, _result):
        http = run.since(mark, kind="http")
        return not http, {"http_requests": len(http)}
    run.check("B10", "tls, not_sent; no HTTP request completed",
              failure_case("tls-untrusted", "tls", "not_sent", no_http))
    run.check("B11", "tls, not_sent; no HTTP request completed",
              failure_case("tls-wrong-name", "tls", "not_sent", no_http))

    def target_untouched(mark, _result):
        hits = run.since(mark, kind="connect", role="redirect_target")
        return not hits, {"redirect_target_connections": len(hits)}
    run.check("B12", "redirect_refused; redirect target untouched",
              failure_case("redirect", "redirect_refused", "sent", target_untouched))

    def b13():
        run.scenario("auth-failure")
        run.restart_provider()
        first = run.submit("auth-1")
        ok_first, err_first = expect_failure(first, "auth_failed", "sent")
        mark = run.mark()
        second = run.submit("auth-2")
        ok_second, err_second = expect_failure(second, "auth_failed", "not_sent")
        sent = run.since(mark, kind="http")
        return ok_first and ok_second and not sent, {"first": err_first, "second": err_second,
                                                     "requests_after_failure": len(sent)}
    run.check("B13", "auth_failed, then refused without sending", b13)

    def retry_after(_mark, result):
        error = ((result.get("final") or {}).get("error") or {})
        return error.get("retry_after_ms") == 7000, {"retry_after_ms": error.get("retry_after_ms")}
    run.check("B14", "rate_limited with retry_after_ms 7000",
              failure_case("rate-limited", "rate_limited", "sent", retry_after))

    def one_chat(mark, _result):
        chats = [e for e in run.since(mark, kind="http") if "chat" in e["path"]]
        return len(chats) == 1, {"chat_requests": len(chats)}
    run.check("B15", "server_error, outcome unknown, one chat request (no retry)",
              failure_case("server-error", "server_error", "unknown", one_chat))

    def no_chat(mark, _result):
        chats = [e for e in run.since(mark, kind="http") if "chat" in e["path"]]
        return not chats, {"chat_requests": len(chats)}
    run.check("B16", "model_unavailable, not_sent; no chat request",
              failure_case("missing-model", "model_unavailable", "not_sent", no_chat))
    run.check("B17", "model_mismatch", failure_case("model-mismatch", "model_mismatch", "sent"))
    run.check("B18", "oversized response: invalid_response",
              failure_case("oversized", "invalid_response", "sent"))
    run.check("B19", "malformed JSON: invalid_response", failure_case("malformed-json", "invalid_response", "sent"))
    run.check("B20", "tool_calls: unsupported_response",
              failure_case("tool-calls", "unsupported_response", "sent"))

    def success(scenario, check_final):
        def body():
            run.scenario(scenario)
            run.restart_provider()
            result = run.submit(scenario)
            final = result.get("final") or {}
            return final.get("state") == "succeeded" and check_final(final), {
                key: final.get(key) for key in ("state", "usage", "latency_ms", "text_length", "model_used")}
        return body
    run.check("B21", "succeeded with usage source unknown and null counts",
              success("missing-usage", lambda f: (f.get("usage") or {}).get("source") == "unknown"
                      and (f.get("usage") or {}).get("tokens_out") is None))
    run.check("B22", "outcome unknown after a dropped body",
              failure_case("unknown-outcome", "transport", "unknown"))
    run.check("B23", "delayed reply succeeds; latency >= 5000 ms",
              success("delayed", lambda f: (f.get("latency_ms") or 0) >= 5000))

    def b24():
        run.scenario("ok")
        run.restart_provider()
        mark = run.mark()
        request_id = f"kvm-dup-{secrets.token_hex(6)}"
        first = run.submit("dup", request_id=request_id)
        again = run.submit("dup", request_id=request_id)
        other = run.submit("dup", request_id=request_id, variant=1)
        chats = [e for e in run.since(mark, kind="http") if "chat" in e["path"]]
        ok_other, err_other = expect_failure(other, "request_id_reused", "not_sent")
        passed = ((first.get("final") or {}).get("state") == "succeeded" and len(chats) == 1 and ok_other
                  and (again.get("final") or {}).get("state") == "succeeded")
        return passed, {"chat_requests": len(chats), "different_arguments": err_other}
    run.check("B24", "one chat for a repeated id; different arguments refused", b24)

    def b25():
        run.scenario("ok")
        # The same ExecStart the operator's preflight writes.
        run.runtime_dropin("50-discover-only.conf", (
            f"[Service]\nExecStart=\nExecStart={LIBEXEC}/providerd --config /etc/flowerly/providers/{INSTANCE}"
            f".service.toml --remote-config /etc/flowerly/providers/{INSTANCE}.toml --require-isolated-identity "
            "--discover-only\n"))
        try:
            run.restart_provider()
            mark = run.mark()
            result = run.submit("discover-only")
            ok, error = expect_failure(result, "discover_only", "not_sent")
            chats = [e for e in run.since(mark, kind="http") if "chat" in e["path"]]
        finally:
            run.stop_provider()
            run.remove_runtime_dropin("50-discover-only.conf")
        return ok and not chats, {"error": error, "chat_requests": len(chats)}
    run.check("B25", "discover_only, not_sent; no chat request", b25)
    run.stop_provider()

    def b26():
        journal = sh("journalctl", "--no-pager", "-o", "cat", "-u", UNIT, check=False).stdout
        texts = {"journal": journal}
        for path in EVIDENCE_DIR.glob("*.json"):
            texts[path.name] = path.read_text()
        found = {name: run.evidence.leaks(text) for name, text in texts.items()}
        found = {name: hits for name, hits in found.items() if hits}
        reply_seen = any("FLOWERLY-SENTINEL-REPLY" in text for text in texts.values())
        return not found and not reply_seen, {"scanned": sorted(texts), "leaks": found}
    run.check("B26", "no sentinel prompt, dummy key, bearer or reply text anywhere", b26)

    def b27():
        gate = pathlib.Path("/etc/flowerly/remote-providers.toml")
        gate.write_text(f'enabled = true\nresponse_replay_seconds = 0\n\n[[provider]]\nname = "{INSTANCE}"\n'
                        f'socket = "{SOCKET}"\ndestination = "{names["mock"]}"\nmodels = ["{MODEL}"]\n')
        os.chmod(gate, 0o644)
        dropin = pathlib.Path("/etc/systemd/system/flowerlyd.service.d/60-kvm-shared-group.conf")
        dropin.parent.mkdir(parents=True, exist_ok=True)
        dropin.write_text("[Service]\nUnsetEnvironment=\nEnvironment=AGENTIC_USERS_GROUP=" + CLIENT_GROUP + "\n"
                          "ExecStart=\nExecStart=/usr/bin/flowerlyd --state-dir /var/lib/flowerly "
                          "--remote-providers /etc/flowerly/remote-providers.toml\n")
        since = utc().replace("T", " ").rstrip("Z")
        try:
            sh("systemctl", "daemon-reload")
            sh("systemctl", "restart", "flowerlyd.service", check=False)
            time.sleep(3)
            journal = run.journal("flowerlyd.service", since)
        finally:
            dropin.unlink(missing_ok=True)
            gate.unlink(missing_ok=True)
            sh("systemctl", "daemon-reload")
            sh("systemctl", "restart", "flowerlyd.service", check=False)
        disabled = "would admit the provider process" in journal
        enabled = "remote inference ENABLED" in journal
        return disabled and not enabled, {"refusal_logged": disabled, "enabled_logged": enabled}
    run.check("B27", "shared group: remote inference disabled", b27)


# --- socket identities --------------------------------------------------------

def part_s(run):
    # Jobs must finish before the next submit: the operator's config allows
    # one request at a time.
    run.scenario("ok")
    run.restart_provider()
    clients = grp.getgrnam(CLIENT_GROUP).gr_gid

    def admitted(user, groups=None, extra=()):
        return run.client_json(user, "admission", SOCKET, *extra, groups=groups)

    def flowerlyd_groups():
        pid = sh("systemctl", "show", "-p", "MainPID", "--value", "flowerlyd.service").stdout.strip()
        status = pathlib.Path(f"/proc/{pid}/status").read_text()
        return re.search(r"(?m)^Groups:\s*(.*)$", status).group(1).split()

    def s1():
        result = admitted(SERVICE_USER, groups=[CLIENT_GROUP])
        return result["admitted"] is True, result
    run.check("S1", "admitted", s1)

    def s2():
        flag = WORK / "s2-go"
        flag.unlink(missing_ok=True)
        entry = pwd.getpwnam(SERVICE_USER)
        # initgroups now: flowerly is not a member yet, so the process lacks the group.
        child = subprocess.Popen(["runuser", "-u", SERVICE_USER, "--", "python3", "-I", str(run.client),
                                  "admission", SOCKET, "--wait-file", str(flag)],
                                 stdout=subprocess.PIPE, text=True)
        time.sleep(1)
        sh("gpasswd", "-a", SERVICE_USER, CLIENT_GROUP)
        try:
            flag.touch()
            out, _ = child.communicate(timeout=120)
        finally:
            sh("gpasswd", "-d", SERVICE_USER, CLIENT_GROUP, check=False)
        result = json.loads(out)
        return (result["admitted"] is False and str(clients) not in result.get("identity", {}).get("Groups", []),
                dict(result, uid=entry.pw_uid))
    run.check("S2", "refused: listed in /etc/group but not in the process's groups", s2)

    def s3():
        result = admitted(PLAIN_USER, groups=[])
        return result["admitted"] is False and "connect_error" in result, result
    run.check("S3", "refused at connect", s3)

    def s4():
        result = admitted(PLAIN_USER, groups=[CLIENT_GROUP])
        return result["admitted"] is True, result
    run.check("S4", "admitted: effective membership is the boundary", s4)

    def s5():
        result = admitted("root", groups=[])
        # Root passes the 0660 file mode; the provider's peer check must refuse.
        return result["admitted"] is False and "refused_after_connect" in result, result
    run.check("S5", "root without the group refused by the peer check, not the file mode", s5)

    def s6():
        result = admitted("root", groups=[CLIENT_GROUP])
        return result["admitted"] is True, result
    run.check("S6", "root with the group admitted", s6)

    def s7():
        result = admitted(PROVIDER_USER)
        return result["admitted"] is True, result
    run.check("S7", "service account admitted", s7)

    def s8_s9():
        a = run.submit("owner-a")
        request_id = a["request_id"]
        b_poll = run.client_json(PLAIN_USER, "poll", SOCKET, request_id, groups=[CLIENT_GROUP])
        b_cancel = run.client_json(PLAIN_USER, "cancel", SOCKET, request_id, groups=[CLIENT_GROUP])
        a_poll = run.client_json(PROVIDER_USER, "poll", SOCKET, request_id)
        return a, b_poll, b_cancel, a_poll

    def s8():
        a, b_poll, _b_cancel, a_poll = s8_s9()
        own = a_poll.get("type") == "ok"
        foreign = (b_poll.get("error") or {}).get("class") == "unknown_request"
        return own and foreign, {"owner_poll": a_poll.get("type"), "other_poll": b_poll.get("error")}
    run.check("S8", "the same request id is a different job for another uid", s8)

    def s9():
        a, b_poll, b_cancel, a_poll = s8_s9()
        refused = all((r.get("error") or {}).get("class") == "unknown_request" for r in (b_poll, b_cancel))
        still_mine = a_poll.get("state") in ("pending", "succeeded")
        return refused and still_mine, {"other_poll": b_poll.get("error"), "other_cancel": b_cancel.get("error"),
                                        "owner_state_after": a_poll.get("state")}
    run.check("S9", "cross-uid poll and cancel refused; the owner's job untouched", s9)

    def s10():
        before = flowerlyd_groups()
        dropin = pathlib.Path("/etc/systemd/system/flowerlyd.service.d/60-kvm-clients.conf")
        dropin.parent.mkdir(parents=True, exist_ok=True)
        dropin.write_text(f"[Service]\nSupplementaryGroups={CLIENT_GROUP}\n")
        try:
            sh("systemctl", "daemon-reload")
            stale = flowerlyd_groups()
            sh("systemctl", "restart", "flowerlyd.service")
            time.sleep(2)
            after = flowerlyd_groups()
        finally:
            dropin.unlink(missing_ok=True)
            sh("systemctl", "daemon-reload")
            sh("systemctl", "restart", "flowerlyd.service", check=False)
        gid = str(clients)
        return gid not in before and gid not in stale and gid in after, {
            "before": before, "after_daemon_reload": stale, "after_restart": after}
    run.check("S10", "the clients group appears only after a restart", s10)
    run.stop_provider()


# --- Part C, lifecycle ---------------------------------------------------------

def interposed(run, command, after, *args):
    directory = WORK / "interpose"
    directory.mkdir(exist_ok=True)
    wrapper = directory / command
    if not wrapper.exists():
        wrapper.symlink_to(run.repo / "testbed/remote-provider-kvm/guest/interpose.sh")
    counter = WORK / f"interpose-{command}-{secrets.token_hex(4)}"
    return run.operator_cmd(*args, env_extra={
        "PATH": f"{directory}:{os.environ['PATH']}", "FLOWERLY_KVM_INTERPOSE_COUNTER": str(counter),
        "FLOWERLY_KVM_INTERPOSE_AFTER": str(after)})


def part_c(run, before):
    api_base = f"https://{run.network['names']['mock']}:{run.network['mock_port']}"
    ledger = pathlib.Path("/var/lib/flowerly-provider-operator")
    shared = [str(LIBEXEC / "providerd"), "/etc/systemd/system/flowerly-provider-remote@.service",
              "/etc/sysusers.d/flowerly-provider.conf"]

    def shared_state():
        return {path: sha256(path) if os.path.exists(path) else None for path in shared} | {
            "account": bool(sh("getent", "passwd", PROVIDER_USER, check=False).stdout),
            "group": bool(sh("getent", "group", CLIENT_GROUP, check=False).stdout)}

    def c3():
        result = run.operator_cmd("install", "bad", "http://plaintext.flowerly.test", "open-webui", MODEL)
        leftovers = [p for p in ("/etc/flowerly/providers/bad.toml", "/etc/flowerly/providers/bad.service.toml")
                     if os.path.exists(p)]
        return result.returncode != 0 and not leftovers, {"exit": result.returncode, "leftovers": leftovers}
    run.check("C3", "invalid configuration refused before any change", c3)

    def c4():
        dummy_credential("second")
        installed = run.operator_cmd("install", "second", api_base, "open-webui", MODEL)
        snapshot = shared_state()
        removed = run.operator_cmd("rollback", "second")
        kept = shared_state() == snapshot and os.path.exists(f"/etc/flowerly/providers/{INSTANCE}.toml")
        os.unlink("/etc/credstore.encrypted/second.key")
        return installed.returncode == 0 and removed.returncode == 0 and kept, {
            "install_exit": installed.returncode, "rollback_exit": removed.returncode, "shared_kept": kept}
    run.check("C4", "rolling back one of two instances keeps the other and all shared resources", c4)

    def c11_and_c6():
        config = pathlib.Path(f"/etc/flowerly/providers/{INSTANCE}.toml")
        original = config.read_bytes()
        config.write_bytes(original + b"# edited\n")
        preflight = run.operator_cmd("preflight", INSTANCE, str(EVIDENCE_DIR))
        rollback = run.operator_cmd("rollback", INSTANCE)
        kept = config.exists() and (ledger / "instances" / INSTANCE).exists()
        config.write_bytes(original)
        return preflight, rollback, kept

    results = {}

    def c11():
        preflight, rollback, kept = c11_and_c6()
        results["c6"] = (rollback, kept)
        return preflight.returncode != 0 and kept, {"preflight_exit": preflight.returncode,
                                                    "rollback_exit": rollback.returncode, "file_and_ledger_kept": kept}
    run.check("C11", "modified file kept, ledger keeps it, preflight refuses", c11)

    def c6():
        if "c6" not in results:
            raise Harness("C11 did not run")
        rollback, kept = results["c6"]
        rerun = run.operator_cmd("rollback", INSTANCE)
        gone = not os.path.exists(f"/etc/flowerly/providers/{INSTANCE}.toml") and \
            not (ledger / "instances" / INSTANCE).exists()
        return rollback.returncode != 0 and kept and rerun.returncode == 0 and gone, {
            "partial_exit": rollback.returncode, "rerun_exit": rerun.returncode, "instance_gone": gone}
    run.check("C6", "partial rollback keeps what it could not remove; rerun completes", c6)

    def reinstall(name=INSTANCE):
        result = run.operator_cmd("install", name, api_base, "open-webui", MODEL)
        if result.returncode != 0:
            raise Harness(f"reinstall {name} failed: {result.stderr[-200:]}")

    def c13():
        reinstall()
        holder = subprocess.Popen(["flock", "/run/flowerly-provider-operator.lock", "sleep", "60"])
        time.sleep(1)
        try:
            before_state = run.snapshot()
            refused = run.operator_cmd("rollback", INSTANCE)
            unchanged = run.snapshot() == before_state
        finally:
            holder.kill()
            holder.wait()
        return refused.returncode != 0 and unchanged, {"exit": refused.returncode, "unchanged": unchanged}
    run.check("C13", "second command refuses on the lock with no change", c13)

    def c12():
        entry = ledger / "instances" / INSTANCE
        original = entry.read_bytes()
        entry.write_bytes(original + b"file\tetc/passwd\t0000\n")
        try:
            before_state = run.snapshot()
            refused = run.operator_cmd("rollback", INSTANCE)
            unchanged = {k: v for k, v in run.snapshot().items()} == before_state
        finally:
            entry.write_bytes(original)
        return refused.returncode != 0 and unchanged, {"exit": refused.returncode, "unchanged": unchanged}
    run.check("C12", "corrupt ledger refuses with no change", c12)

    def c10():
        config = pathlib.Path(f"/etc/flowerly/providers/{INSTANCE}.toml")
        aside = WORK / "config-aside"
        shutil.move(config, aside)
        decoy = WORK / "symlink-target"
        decoy.write_text("must survive\n")
        config.symlink_to(decoy)
        try:
            refused = run.operator_cmd("rollback", INSTANCE)
            survived = decoy.read_text() == "must survive\n" and config.is_symlink()
        finally:
            config.unlink(missing_ok=True)
            shutil.move(aside, config)
        return refused.returncode != 0 and survived, {"exit": refused.returncode, "target_untouched": survived}
    run.check("C10", "symlinked recorded path kept; its target untouched", c10)

    def c8():
        # rm call 1 removes the (absent) discover-only drop-in; call 2 is the
        # first recorded instance file.
        killed = interposed(run, "rm", 2, "rollback", INSTANCE)
        rerun = run.operator_cmd("rollback", INSTANCE)
        gone = not (ledger / "instances" / INSTANCE).exists()
        return killed.returncode != 0 and rerun.returncode == 0 and gone, {
            "killed_exit": killed.returncode, "rerun_exit": rerun.returncode, "instance_gone": gone}
    run.check("C8", "rollback killed after its first removal; rerun completes", c8)

    def c15():
        reinstall()
        sleeper = subprocess.Popen(["runuser", "-u", PROVIDER_USER, "--", "sleep", "300"])
        time.sleep(1)
        try:
            before_state = shared_state()
            rolled = run.operator_cmd("rollback", INSTANCE)
            kept = shared_state() == before_state
            refused = run.operator_cmd("rollback-shared")
            still = shared_state() == before_state
        finally:
            sleeper.kill()
            sleeper.wait()
        sh("gpasswd", "-a", SERVICE_USER, CLIENT_GROUP)
        member = run.operator_cmd("rollback-shared")
        member_kept = shared_state() == before_state
        sh("gpasswd", "-d", SERVICE_USER, CLIENT_GROUP)
        final = run.operator_cmd("rollback-shared")
        gone = shared_state() == {**{p: None for p in shared}, "account": False, "group": False}
        results["c5"] = {"process_kept": kept and still and refused.returncode != 0,
                         "member_kept": member_kept and member.returncode != 0}
        return rolled.returncode == 0 and kept and still and member_kept and final.returncode == 0 and gone, {
            "rollback_exit": rolled.returncode, "kept_while_process": kept and still,
            "kept_while_member": member_kept, "final_exit": final.returncode, "shared_gone": gone}
    run.check("C15", "shared resources removed only after the final consumer exits", c15)

    def c5():
        if "c5" not in results:
            raise Harness("C15 did not run")
        return all(results["c5"].values()), results["c5"]
    run.check("C5", "process and group member each keep every shared resource", c5)

    def c7():
        # Power cut right after systemd-sysusers created the accounts.
        killed = interposed(run, "systemd-sysusers", 1, "install", INSTANCE, api_base, "open-webui", MODEL)
        recovery = run.operator_cmd("rollback", INSTANCE)
        shared_after = run.operator_cmd("rollback-shared")
        accounts = bool(sh("getent", "passwd", PROVIDER_USER, check=False).stdout)
        return killed.returncode != 0 and accounts, {
            "killed_exit": killed.returncode, "recovery_exit": recovery.returncode,
            "rollback_shared_exit": shared_after.returncode, "pending_accounts_kept": accounts}
    run.check("C7", "interrupted install: recovery never deletes the pending accounts", c7)

    def manual_cleanup():
        # What C7 deliberately left, removed by hand as an operator would.
        (LIBEXEC / "providerd").unlink(missing_ok=True)
        sh("userdel", PROVIDER_USER, check=False)
        for group in (PROVIDER_USER, CLIENT_GROUP):
            sh("groupdel", group, check=False)
        for rel in ("etc/systemd/system/flowerly-provider-remote@.service", "etc/sysusers.d/flowerly-provider.conf",
                    "usr/local/libexec/flowerly/remote-provider-acceptance.py"):
            pathlib.Path("/" + rel).unlink(missing_ok=True)
        shutil.rmtree(ledger, ignore_errors=True)
        for path in (f"/etc/flowerly/providers/{INSTANCE}.toml", f"/etc/flowerly/providers/{INSTANCE}.service.toml",
                     f"/etc/systemd/system/{UNIT}.d/10-operator.conf"):
            pathlib.Path(path).unlink(missing_ok=True)
        sh("systemctl", "daemon-reload")
    manual_cleanup()

    def c9():
        # A foreign binary identical to the one install would write.
        LIBEXEC.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(run.bundle / "bin/providerd", LIBEXEC / "providerd")
        os.chmod(LIBEXEC / "providerd", 0o755)
        foreign = os.stat(LIBEXEC / "providerd").st_ino
        reinstall()
        rolled = run.operator_cmd("rollback", INSTANCE)
        cleared = run.operator_cmd("rollback-shared")
        kept = (LIBEXEC / "providerd").exists() and os.stat(LIBEXEC / "providerd").st_ino == foreign
        (LIBEXEC / "providerd").unlink(missing_ok=True)
        return kept, {"rollback_exit": rolled.returncode, "rollback_shared_exit": cleared.returncode,
                      "foreign_binary_kept_same_inode": kept}
    run.check("C9", "a foreign identical binary is never removed", c9)

    def c14():
        reinstall()
        gate = pathlib.Path("/etc/flowerly/remote-providers.toml")
        gate.write_text("enabled = false\n")
        os.chmod(gate, 0o644)
        disabled = run.operator_cmd("disable", INSTANCE)
        enabled_state = sh("systemctl", "is-enabled", UNIT, check=False).stdout.strip()
        start = sh("systemctl", "start", UNIT, check=False)
        moved = not gate.exists() and any(pathlib.Path("/etc/flowerly").glob("remote-providers.toml.disabled-*"))
        for path in pathlib.Path("/etc/flowerly").glob("remote-providers.toml.disabled-*"):
            path.unlink()
        rolled = run.operator_cmd("rollback", INSTANCE)
        cleared = run.operator_cmd("rollback-shared")
        return (disabled.returncode == 0 and enabled_state == "masked" and start.returncode != 0 and moved), {
            "disable_exit": disabled.returncode, "is_enabled": enabled_state, "start_exit": start.returncode,
            "gate_moved": moved, "rollback_exit": rolled.returncode, "rollback_shared_exit": cleared.returncode}
    run.check("C14", "disable: masked, cannot start, gate moved aside", c14)

    def c16():
        pathlib.Path(f"/etc/credstore.encrypted/{INSTANCE}.key").unlink(missing_ok=True)
        after = run.snapshot()
        differences = {key: sorted(set(map(str, after[key])) ^ set(map(str, before[key])))
                       for key in before if after[key] != before[key]}
        if "files" in differences:
            differences["files"] = sorted(set(after["files"].items()) ^ set(before["files"].items()))
        return not differences, {"differences": differences}
    run.check("C16", "guest matches the pre-install snapshot", c16)


def main(argv):
    if len(argv) != 2:
        sys.exit(f"usage: {argv[0]} <bundle-dir>")
    early_guard()
    run = Run(argv[1])
    guard(run)
    WORK.mkdir(mode=0o755, exist_ok=True)
    collect_facts(run)
    check_environment(run)
    try:
        setup_network(run)
        setup_fixtures(run)
        setup_pki_and_mock(run)
        before = run.snapshot()
        install_provider(run)
        part_a(run)
        part_b(run)
        part_s(run)
        part_c(run, before)
    except Harness as err:
        print(f"stopped: {err}", file=sys.stderr)
    except Exception:  # noqa: BLE001 - record what ran, never a pass
        traceback.print_exc()
    finally:
        sh("systemctl", "stop", "flowerly-kvm-mock.service", check=False)
    return run.write()


if __name__ == "__main__":
    sys.exit(main(sys.argv))

#!/usr/bin/env python3
"""#225 PR 8 acceptance with real systemd: default and opt-in admission.

Run as root on a DISPOSABLE systemd host (the Arch guest of
arch-kernel-vm-test.sh): it installs the built flowerlyd, the shipped unit,
sysusers file and multi-user drop-in into the real system paths, creates
human test accounts, and drives flowerlyd only through systemctl. It
refuses to run unless FLOWERLY_DISPOSABLE_HOST=1.

  1. Default (unit alone): only the flowerly service account can connect.
     /run/flowerly is 0700, the socket 0600, state private. Users members,
     human admins, admin-only members and non-members are refused, also
     with a manager-injected users group, config file or database path
     (#237: the daemon must keep /var/lib/flowerly/state.db, never
     /dev/shm) and a stale partial drop-in. The service account is not an
     administrator.
  2. Opt-in (the documented drop-in installed as documented): flowerly-users
     members connect; the socket is 0660 group flowerly-users, /run/flowerly
     0750, state still 0700/0600. The full two-user ownership suite from
     test-user-ownership.py runs against the real socket, including restart
     persistence, Generations and the audited ownership claim. A human
     administrator (users + admin groups) runs Admin commands and is
     journaled and audited by their own uid; unprivileged users cannot; no
     one but the service account gets a PTY; admin-only and non-members
     cannot connect. A manager-injected users group or config is ignored.
  3. Disable (drop-in removed as documented): service-account-only again,
     with the data intact and the state re-owned by the private group.

Prints an evidence report (systemd policy under test, effective permissions,
results) to stdout.
"""
import argparse
import grp
import importlib.util
import json
import os
import pathlib
import pwd
import shutil
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
OVERLAY = ROOT / "distro/overlay"
UNIT_SRC = OVERLAY / "usr/lib/systemd/system/flowerlyd.service"
SYSUSERS_SRC = OVERLAY / "usr/lib/sysusers.d/flowerly.conf"
DROP_IN_SRC = OVERLAY / "usr/share/flowerly/systemd/flowerlyd-multi-user.conf"

UNIT = pathlib.Path("/usr/lib/systemd/system/flowerlyd.service")
SYSUSERS = pathlib.Path("/usr/lib/sysusers.d/flowerly.conf")
SHARED_DROP_IN = pathlib.Path("/usr/share/flowerly/systemd/flowerlyd-multi-user.conf")
DROP_IN_DIR = pathlib.Path("/etc/systemd/system/flowerlyd.service.d")
DROP_IN = DROP_IN_DIR / "50-multi-user.conf"
STALE_DROP_IN = DROP_IN_DIR / "10-stale.conf"
BINARY = pathlib.Path("/usr/bin/flowerlyd")
RUNTIME = pathlib.Path("/run/flowerly")
STATE = pathlib.Path("/var/lib/flowerly")
SOCKET = RUNTIME / "flowerlyd.sock"
DB = STATE / "state.db"
EVIL_CONFIG = pathlib.Path("/etc/flowerly-injected.toml")
# Writable for the sandboxed daemon despite ProtectSystem=strict (#237).
EVIL_DB = pathlib.Path("/dev/shm/flowerly-injected.db")
SERVICE = "flowerly"
USERS, ADMINS, EVIL = "flowerly-users", "flowerly-admin", "flwt-evil"
ALICE, BOB, DANA, CAROL, MALLORY = (f"flwt-{n}" for n in ("alice", "bob", "dana", "carol", "mallory"))
MANAGER_ENV = [f"AGENTIC_USERS_GROUP={EVIL}", f"AGENTIC_CONFIG={EVIL_CONFIG}",
               f"AGENTIC_DATABASE_PATH={EVIL_DB}",
               "AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP=members-can-read-each-others-data"]
ADMIN_COMMANDS = ["generation.activate", "generation.promote", "model.register",
                  "ownership.inspect", "ownership.claim", "ownership.release",
                  "provenance.add", "provider.register", "usage.record"]


def sh(*argv, check=True):
    return subprocess.run(argv, check=check, capture_output=True, text=True)


def report(section, text):
    print(f"\n== {section} ==\n{text.rstrip()}", flush=True)


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def mode_of(path):
    meta = path.lstat()
    return (stat.S_IMODE(meta.st_mode), pwd.getpwuid(meta.st_uid).pw_name,
            grp.getgrgid(meta.st_gid).gr_name)


class Host:
    def __init__(self, ownership, client_script):
        self.own = ownership
        self.script = client_script

    def raw(self, user, payload):
        """One request as `user`; outcome reply / eacces / closed."""
        result = subprocess.run(
            ["runuser", "-u", user, "--", "env", "-i", "PATH=/usr/bin:/bin", sys.executable,
             "-I", str(self.script), "--client", str(SOCKET), json.dumps(payload)],
            check=True, capture_output=True, text=True, timeout=30)
        return json.loads(result.stdout)

    def peer(self, user, spoof_user):
        return self.own.Peer(user, self.script, SOCKET, pwd.getpwnam(spoof_user).pw_uid)

    def restart(self):
        sh("systemctl", "daemon-reload")
        sh("systemctl", "restart", "flowerlyd")
        for _ in range(300):
            if SOCKET.exists():
                try:
                    if self.raw(SERVICE, {"type": "ping"})["outcome"] == "reply":
                        return
                except subprocess.CalledProcessError:
                    pass
            time.sleep(0.1)
        status = sh("systemctl", "status", "--no-pager", "flowerlyd", check=False).stdout
        raise AssertionError(f"flowerlyd not ready after restart:\n{status}")

    def as_daemon(self, *argv):
        """Run argv with the daemon's own uid and gid (no ptrace capability
        needed to read its /proc entries)."""
        pid = sh("systemctl", "show", "-p", "MainPID", "--value", "flowerlyd").stdout.strip()
        assert pid not in ("", "0"), pid
        status = pathlib.Path(f"/proc/{pid}/status").read_text()
        gid = int(next(l for l in status.splitlines() if l.startswith("Gid:")).split()[1])
        return subprocess.run(["runuser", "-u", SERVICE, "-g", grp.getgrgid(gid).gr_name, "--",
                               *[a.replace("{pid}", pid) for a in argv]],
                              check=True, capture_output=True).stdout

    def environ(self):
        raw = self.as_daemon("cat", "/proc/{pid}/environ").split(b"\0")
        return dict(item.decode().partition("=")[::2] for item in raw if item)

    def open_databases(self):
        """The *.db files the daemon has open (#237)."""
        listing = self.as_daemon("find", "/proc/{pid}/fd", "-lname", "*.db", "-printf", "%l\n")
        return set(listing.decode().split())

    def status(self):
        reply = self.raw(SERVICE, {"type": "status"})
        assert reply["outcome"] == "reply" and reply["reply"]["type"] == "ok", reply
        return reply["reply"]["data"]["details"]

    def command(self, user, name, **args):
        return {"type": "command", "name": name, "args": dict({"admin": True, "uid": 0}, **args)}

    def denied(self, user, name, **args):
        outcome = self.raw(user, self.command(user, name, **args))
        assert outcome["outcome"] == "reply", (user, name, outcome)
        reply = outcome["reply"]
        return reply["type"] == "err" and reply.get("code") == "permission_denied"

    def refused(self, user):
        """The filesystem refuses `user`, and so does the kernel check when the
        directory and socket modes are deliberately opened up."""
        assert self.raw(user, {"type": "ping"})["outcome"] == "eacces", user
        runtime_mode, socket_mode = mode_of(RUNTIME)[0], mode_of(SOCKET)[0]
        RUNTIME.chmod(0o755)
        SOCKET.chmod(0o666)
        try:
            assert self.raw(user, self.command(user, "flow.list"))["outcome"] == "closed", user
        finally:
            SOCKET.chmod(socket_mode)
            RUNTIME.chmod(runtime_mode)


def check_state_private():
    for path in [STATE, *STATE.iterdir()]:
        mode, owner, _ = mode_of(path)
        assert owner == SERVICE and mode & 0o077 == 0, (path, oct(mode), owner)


def policy_evidence(label):
    shown = sh("systemctl", "show", "flowerlyd", "-p",
               "User,Group,RuntimeDirectoryMode,StateDirectoryMode,UMask,Environment,"
               "UnsetEnvironment,DropInPaths").stdout
    perms = "\n".join(f"{p}: mode={oct(m)} owner={o} group={g}"
                      for p in (RUNTIME, SOCKET, STATE, DB) for m, o, g in [mode_of(p)])
    report(f"{label}: effective systemd policy", shown + "\n" + perms)


def default_phase(host, label):
    host.restart()
    assert mode_of(RUNTIME) == (0o700, SERVICE, SERVICE), mode_of(RUNTIME)
    assert mode_of(SOCKET) == (0o600, SERVICE, SERVICE), mode_of(SOCKET)
    check_state_private()
    env = host.environ()
    assert host.open_databases() == {str(DB)}, host.open_databases()
    assert not EVIL_DB.exists(), EVIL_DB
    for name in ("AGENTIC_USERS_GROUP", "AGENTIC_CONFIG", "AGENTIC_DATABASE_PATH"):
        assert name not in env, (label, name, env.get(name))
    status = host.status()
    assert status["client_access"] == "service_account_only", status
    assert status["admin_authorization"] == "linux_group", status
    for user in (ALICE, BOB, DANA, CAROL, MALLORY):
        host.refused(user)
    for name in ADMIN_COMMANDS:
        assert host.denied(SERVICE, name), name
    policy_evidence(label)
    report(label, "service-account-only: users members, human admin, admin-only member and "
           "non-member refused at filesystem and kernel check; state private; service "
           "account not admin: PASS")


def opt_in_phase(host, ownership, authorization):
    started = time.strftime("%Y-%m-%d %H:%M:%S")
    host.restart()
    assert mode_of(RUNTIME) == (0o750, SERVICE, USERS), mode_of(RUNTIME)
    assert mode_of(SOCKET) == (0o660, SERVICE, USERS), mode_of(SOCKET)
    check_state_private()
    for entry in RUNTIME.iterdir():
        if entry != SOCKET:
            assert mode_of(entry)[0] & 0o007 == 0, (entry, oct(mode_of(entry)[0]))
    env = host.environ()
    # The drop-in's own value wins over the manager-injected one.
    assert env.get("AGENTIC_USERS_GROUP") == USERS, env.get("AGENTIC_USERS_GROUP")
    assert "AGENTIC_CONFIG" not in env, env.get("AGENTIC_CONFIG")
    assert "AGENTIC_DATABASE_PATH" not in env, env.get("AGENTIC_DATABASE_PATH")
    assert host.open_databases() == {str(DB)}, host.open_databases()
    assert not EVIL_DB.exists(), EVIL_DB
    status = host.status()
    assert status["client_access"] == "service_account_and_linux_groups", status
    assert status["ownership_pending_commands"] == 0, status
    policy_evidence("opt-in")

    for user in (CAROL, MALLORY):
        host.refused(user)
    for name in ADMIN_COMMANDS:
        assert host.denied(ALICE, name), name
        assert not host.denied(DANA, name), name
        assert host.denied(SERVICE, name), name
    for user in (ALICE, DANA):
        assert host.denied(user, "session.spawn"), user
        assert host.denied(user, "session.status"), user
    assert not host.denied(SERVICE, "session.status"), "service account PTY control"

    alice, bob, dana = (host.peer(ALICE, BOB), host.peer(BOB, ALICE), host.peer(DANA, ALICE))
    uid = {name: pwd.getpwnam(name).pw_uid for name in (ALICE, BOB, DANA)}
    ownership.ownership_scenario(alice, bob, uid[ALICE], uid[BOB], DB, host.restart)
    ownership.compute_scenario(alice, bob, dana)
    ownership.generation_scenario(alice, bob, dana, uid[DANA])
    ownership.claim_scenario(alice, bob, dana, uid[DANA], uid[ALICE], SERVICE, DB)

    # Audit history and ownership survive a restart; the journal and the
    # audit table name the human administrator's own uid.
    host.restart()
    db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    try:
        actors = {row[0] for row in db.execute("SELECT DISTINCT actor_uid FROM admin_audit")}
        audited = db.execute("SELECT COUNT(*) FROM admin_audit").fetchone()[0]
    finally:
        db.close()
    assert actors == {uid[DANA]} and audited >= 4, (actors, audited)
    journal = sh("journalctl", "-u", "flowerlyd", "--since", started, "-o", "cat",
                 "--no-pager").stdout
    records = authorization.audit_fields(journal)
    granted = [f for f in records
               if f.get("peer_uid") == str(uid[DANA]) and f.get("outcome") != "denied"]
    assert granted and all(f.get("service_account") == "false" for f in granted), records[-20:]
    assert not any(f.get("peer_uid") == str(uid[ALICE]) and f.get("outcome") != "denied"
                   for f in records), records[-20:]
    assert alice.status()["foreground_flow"]["name"] == "General"
    report("opt-in", "flowerly-users members admitted with per-uid ownership (Flows, Goals, "
           "Sessions, Executions, provenance, Compute, Generations, foreground across restart, "
           "audited claim/release); human admin runs audited Admin commands journaled by own "
           f"uid ({audited} admin_audit rows, actor uid {uid[DANA]}); users members refused "
           "Admin; PTY only for the service account; admin-only member and non-member refused; "
           "manager-injected users group, config and database path ignored; state private: PASS")


def install(bin_dir):
    for source, target, mode in [(bin_dir / "flowerlyd", BINARY, 0o755),
                                 (UNIT_SRC, UNIT, 0o644), (SYSUSERS_SRC, SYSUSERS, 0o644),
                                 (DROP_IN_SRC, SHARED_DROP_IN, 0o644)]:
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(mode)
    sh("systemd-sysusers", str(SYSUSERS))
    service = pwd.getpwnam(SERVICE)
    members = [g.gr_name for g in grp.getgrall() if SERVICE in g.gr_mem]
    assert members == [], f"{SERVICE} must have no supplementary groups: {members}"
    assert service.pw_uid < 1000 and service.pw_gid == grp.getgrnam(SERVICE).gr_gid, service
    sh("groupadd", EVIL)
    for user, groups in [(ALICE, USERS), (BOB, USERS), (DANA, f"{USERS},{ADMINS}"),
                         (CAROL, ADMINS), (MALLORY, EVIL)]:
        sh("useradd", "--no-create-home", "--shell", shutil.which("nologin") or "/usr/sbin/nologin",
           "--groups", groups, user)
    shutil.rmtree(STATE, ignore_errors=True)


def cleanup():
    sh("systemctl", "stop", "flowerlyd", check=False)
    sh("systemctl", "unset-environment", *[e.split("=")[0] for e in MANAGER_ENV], check=False)
    for path in (DROP_IN, STALE_DROP_IN, EVIL_CONFIG, EVIL_DB):
        path.unlink(missing_ok=True)
    for user in (ALICE, BOB, DANA, CAROL, MALLORY):
        sh("userdel", user, check=False)
    sh("groupdel", EVIL, check=False)
    sh("systemctl", "daemon-reload", check=False)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("bin_dir")
    args = parser.parse_args()
    if os.geteuid() != 0 or os.environ.get("FLOWERLY_DISPOSABLE_HOST") != "1":
        raise SystemExit("run as root on a disposable host with FLOWERLY_DISPOSABLE_HOST=1: "
                         "this installs flowerlyd, its unit and test accounts system-wide")
    if pathlib.Path("/proc/1/comm").read_text().strip() != "systemd":
        raise SystemExit("PID 1 is not systemd")
    bin_dir = pathlib.Path(args.bin_dir).resolve()
    ownership = load("ownership", ROOT / "scripts/test-user-ownership.py")
    authorization = load("authorization", ROOT / "scripts/test-admin-authorization.py")
    work = pathlib.Path(tempfile.mkdtemp(prefix="flowerly-systemd-"))
    work.chmod(0o755)
    client = work / "client.py"
    shutil.copyfile(ROOT / "scripts/test-user-ownership.py", client)
    client.chmod(0o644)
    host = Host(ownership, client)
    report("host", "\n".join([
        f"commit={os.environ.get('FLOWERLY_TEST_SHA', 'unknown')}",
        f"kernel={os.uname().release}",
        f"systemd={sh('systemctl', '--version').stdout.splitlines()[0]}",
        f"os={sh('sh', '-c', '. /etc/os-release && echo $PRETTY_NAME').stdout.strip()}"]))
    try:
        install(bin_dir)
        default_phase(host, "default")

        # Adversarial default: a manager-injected users group, config file and
        # removed test bypass, plus a stale partial drop-in, change nothing.
        EVIL_CONFIG.write_text(f'users_group = "{EVIL}"\n')
        sh("systemctl", "set-environment", *MANAGER_ENV)
        DROP_IN_DIR.mkdir(parents=True, exist_ok=True)
        STALE_DROP_IN.write_text("[Service]\nEnvironment=AGENTIC_USERS_GROUP=flowerly-users\n"
                                 "Environment=AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP="
                                 "members-can-read-each-others-data\n")
        default_phase(host, "default with injected manager environment and stale drop-in")
        STALE_DROP_IN.unlink()

        # Opt-in exactly as documented (manager injection still present).
        sh("install", "-D", "-m", "0644", str(SHARED_DROP_IN), str(DROP_IN))
        sh("systemctl", "daemon-reload")
        report("opt-in drop-in", sh("systemctl", "cat", "flowerlyd").stdout)
        opt_in_phase(host, ownership, authorization)

        # Disable exactly as documented.
        DROP_IN.unlink()
        default_phase(host, "disabled again")
        db = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
        try:
            kept = db.execute("SELECT COUNT(*) FROM flows WHERE owner_class = 'user'").fetchone()[0]
        finally:
            db.close()
        assert kept >= 2, kept
        assert mode_of(STATE)[2] == SERVICE, mode_of(STATE)
        report("disabled again", f"data intact ({kept} user Flows), state re-owned by group "
               f"{SERVICE}: PASS")
    finally:
        cleanup()
        shutil.rmtree(work, ignore_errors=True)
    print("\nSYSTEMD_MULTI_USER_ADMISSION=PASS (default service-account-only, opt-in drop-in, "
          "disable)")


if __name__ == "__main__":
    main()

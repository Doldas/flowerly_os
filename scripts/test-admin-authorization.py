#!/usr/bin/env python3
"""#31 acceptance: Linux group administrative authorization in flowerlyd.

Default mode (any unprivileged user) runs the real supervisor as the
current user and proves:
  * with no admin group, every Admin command and IPC shutdown is refused
    with permission_denied, even for the service account, and spoofed JSON
    identity fields change nothing;
  * with an admin group the caller is not in, the same holds;
  * an explicit grant of the caller's own gid admits Admin commands and
    shutdown;
  * an unknown group name is a startup error, not a silent downgrade;
  * a users group starts (every User command is owner-scoped, #225).

--cross-uid (root, used by CI through sudo) creates a dedicated service
account plus users/admin groups and members, runs flowerlyd as the service
account and connects as each identity:
  * a users-group member runs User commands but no Admin, PTY or shutdown;
  * a human administrator (users and admin groups) runs Admin commands and
    shutdown, but no PTY, and the Admin journal names their own uid;
  * an admin-group member who is not a users member cannot connect;
  * the service account is never implicitly admin;
  * a non-member is refused by the kernel credential check even when the
    socket file is made world-accessible.
It then starts flowerlyd exactly as the shipped systemd unit configures it,
first alone (the 0.0.4 default: service-account-only at the filesystem and
the kernel check, no implicit admin) and then merged with the opt-in
multi-user drop-in (users members admitted, admin-only and non-members
refused, state still private). Real systemd is accepted in the Arch guest by
test-systemd-multi-user.py.
"""
import argparse
import grp
import importlib.util
import json
import os
import pathlib
import pwd
import re
import shutil
import signal
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import uuid

ADMIN_COMMANDS = [
    "provenance.add",
    "generation.activate",
    "generation.promote",
    "ownership.inspect",
    "ownership.claim",
    "ownership.release",
    "model.register",
    "model.retire",
    "provider.register",
    "usage.record",
]
# Forged identity claims that must never influence authorization.
SPOOFED = {"admin": True, "created_by": "uid:0", "uid": 0, "groups": [0], "role": "admin"}



def recv_exact(conn, count):
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise ConnectionResetError("IPC connection closed during response")
        data.extend(chunk)
    return bytes(data)


def rpc(socket_path, payload):
    correlation = str(uuid.uuid4())
    message = json.dumps({"version": 1, "id": correlation, **payload}).encode("utf-8")
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(12)
        client.connect(str(socket_path))
        client.sendall(struct.pack("<I", len(message)) + message)
        size = struct.unpack("<I", recv_exact(client, 4))[0]
        reply = json.loads(recv_exact(client, size))
    assert reply["id"] == correlation, reply
    return reply


def command(name, args=None):
    return {"type": "command", "name": name, "args": dict(SPOOFED, **(args or {}))}


def client_main(socket_path, payload):
    """One request as the invoking identity; prints a JSON outcome."""
    try:
        reply = rpc(socket_path, json.loads(payload))
        print(json.dumps({"outcome": "reply", "reply": reply}))
    except PermissionError:
        print(json.dumps({"outcome": "eacces"}))
    except (ConnectionResetError, BrokenPipeError):
        print(json.dumps({"outcome": "closed"}))


ANSI = re.compile(r"\x1b\[[0-9;]*m")


def audit_fields(log_text):
    """Fields of each `admin command` journal record."""
    records = []
    for line in ANSI.sub("", log_text).splitlines():
        if "admin command" not in line:
            continue
        fields = dict(re.findall(r'(\w+)=("[^"]*"|\S+)', line))
        records.append({key: value.strip('"') for key, value in fields.items()})
    return records


def audit_records(log_text):
    """(command, outcome, service_account) of each `admin command` journal record."""
    return [(f.get("command", ""), f.get("outcome", ""), f.get("service_account"))
            for f in audit_fields(log_text)]


def is_denied(reply):
    return reply["type"] == "err" and reply.get("code") == "permission_denied"


def wait_ready(socket_path, process, log_path):
    for _ in range(200):
        if process.poll() is not None:
            raise AssertionError(f"flowerlyd exited early: {log_path.read_text()}")
        if socket_path.exists():
            try:
                if rpc(socket_path, {"type": "ping"})["type"] == "ok":
                    return
            except OSError:
                pass
        time.sleep(0.05)
    raise AssertionError(f"flowerlyd not ready: {log_path.read_text()}")


def start(bin_dir, root, label, extra_env):
    runtime = root / f"{label}-runtime"
    state = root / f"{label}-state"
    runtime.mkdir(mode=0o700)
    state.mkdir(mode=0o700)
    log_path = root / f"{label}.log"
    env = dict(os.environ, AGENTIC_RUNTIME_DIR=str(runtime), AGENTIC_STATE_DIR=str(state))
    env.pop("AGENTIC_USERS_GROUP", None)
    env.pop("AGENTIC_ADMIN_GROUP", None)
    env.update(extra_env)
    with log_path.open("wb") as log:
        process = subprocess.Popen([str(bin_dir / "flowerlyd")], env=env,
                                   stdout=log, stderr=subprocess.STDOUT)
    return process, runtime / "flowerlyd.sock", log_path


def stop(process):
    if process.poll() is None:
        process.send_signal(signal.SIGTERM)
        process.wait(timeout=15)


def assert_all_admin_refused(sock, label):
    for name in ADMIN_COMMANDS:
        reply = rpc(sock, command(name))
        assert is_denied(reply), (label, name, reply)
    reply = rpc(sock, {"type": "shutdown"})
    assert is_denied(reply), (label, "shutdown", reply)
    assert rpc(sock, {"type": "ping"})["type"] == "ok", f"{label}: refused shutdown stopped the daemon"
    assert rpc(sock, command("flow.list"))["type"] == "ok", label


def same_uid(bin_dir):
    with tempfile.TemporaryDirectory(prefix="flowerly-admin-authz-") as root:
        root = pathlib.Path(root)
        unused_gid = "3999999999"
        assert int(unused_gid) not in os.getgroups()
        for label, extra, policy in [
            ("unconfigured", {}, "denied_unconfigured"),
            ("other-group", {"AGENTIC_ADMIN_GROUP": unused_gid}, "linux_group"),
        ]:
            process, sock, log = start(bin_dir, root, label, extra)
            try:
                wait_ready(sock, process, log)
                status = rpc(sock, {"type": "status"})["data"]["details"]
                assert status["admin_authorization"] == policy, status
                # An admin group alone never admits other UIDs (#225).
                assert status["client_access"] == "service_account_only", status
                assert os.stat(sock).st_mode & 0o777 == 0o600, oct(os.stat(sock).st_mode)
                assert_all_admin_refused(sock, label)
            finally:
                stop(process)
            assert process.returncode == 0, (label, log.read_text())
            # Every Admin decision is journaled, refusals included (#225).
            records = audit_records(log.read_text())
            for name in ADMIN_COMMANDS + ["shutdown"]:
                assert (name, "denied", "true") in records, (label, name, records)

        process, sock, log = start(bin_dir, root, "granted",
                                   {"AGENTIC_ADMIN_GROUP": str(os.getgid())})
        try:
            wait_ready(sock, process, log)
            for name in ADMIN_COMMANDS:
                reply = rpc(sock, command(name))
                assert not is_denied(reply), (name, reply)
            reply = rpc(sock, {"type": "shutdown"})
            assert reply["type"] == "ok", reply
            process.wait(timeout=15)
        finally:
            stop(process)
        assert process.returncode == 0, log.read_text()
        records = audit_records(log.read_text())
        for name in ADMIN_COMMANDS:
            assert any(c == name and o in ("succeeded", "failed") for c, o, _ in records), (name, records)
        assert ("shutdown", "accepted", "true") in records, records
        # Arguments, including forged identity values, are never journaled.
        assert '"role"' not in log.read_text() and "uid:0" not in log.read_text()

        process, sock, log = start(bin_dir, root, "unknown",
                                   {"AGENTIC_ADMIN_GROUP": "flowerly-no-such-group"})
        try:
            process.wait(timeout=15)
        finally:
            stop(process)
        assert process.returncode != 0, "unknown admin group must refuse to start"
        assert "flowerly-no-such-group" in log.read_text(), log.read_text()

        # Every User command is owner-scoped (#225), so a users group starts
        # without any acknowledgment; the removed test-only bypass is inert.
        process, sock, log = start(bin_dir, root, "users",
                                   {"AGENTIC_USERS_GROUP": str(os.getgid()),
                                    "AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP": "1"})
        try:
            wait_ready(sock, process, log)
            status = rpc(sock, {"type": "status"})["data"]["details"]
            assert status["client_access"] == "service_account_and_linux_groups", status
            assert status["ownership_pending_commands"] == 0, status
            assert os.stat(sock).st_mode & 0o777 == 0o660, oct(os.stat(sock).st_mode)
            assert "TEST ONLY" not in log.read_text(), log.read_text()
        finally:
            stop(process)
        print("same-UID admin authorization: Admin commands and shutdown refused without an "
              "explicit admin group; spoofed JSON ignored; explicit grant admitted; "
              "unknown group refused at startup; owner-scoped users group admitted: OK")


def run(*argv):
    subprocess.run(argv, check=True, stdout=subprocess.DEVNULL)


def as_user(user, script, sock, payload, groups=()):
    result = subprocess.run(
        ["runuser", "-u", user, *groups, "--", "env", "-i", "PATH=/usr/bin:/bin",
         sys.executable, "-I", str(script), "--client", str(sock), json.dumps(payload)],
        check=True, capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout)


SCRIPTS = pathlib.Path(__file__).resolve().parent


def shipped_policy():
    """The offline unit/drop-in parser shared with test-shipped-ipc-admission."""
    spec = importlib.util.spec_from_file_location(
        "shipped_admission", SCRIPTS / "test-shipped-ipc-admission.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def shipped_unit(root, local_bin, script, service, users, admin, admin_only, mallory,
                 users_group, admin_group, opt_in):
    """#225: run flowerlyd exactly as the shipped unit configures it (alone, or
    merged with the opt-in drop-in), with the unit's group names mapped to this
    run's test groups, and check admission at the filesystem and the kernel."""
    policy = shipped_policy()
    texts = [policy.UNIT.read_text()] + ([policy.DROP_IN.read_text()] if opt_in else [])
    values = policy.effective(*texts)
    problems = (policy.opt_in_problems if opt_in else policy.default_problems)(values)
    assert problems == [], problems
    environment = policy.environment_of(values)
    label = "opt-in" if opt_in else "default"
    names = {"flowerly-users": users_group, "flowerly-admin": admin_group, "flowerly": service}
    service_pw = pwd.getpwnam(service)
    group = names[values["Group"][0]]
    gid = grp.getgrnam(group).gr_gid
    runtime, state = root / f"shipped-{label}-runtime", root / f"shipped-{label}-state"
    runtime.mkdir()
    state.mkdir()
    # What systemd does for RuntimeDirectory=/StateDirectory= with User=/Group=.
    os.chown(runtime, service_pw.pw_uid, gid)
    os.chown(state, service_pw.pw_uid, gid)
    runtime_mode = int(values["RuntimeDirectoryMode"][0], 8)
    runtime.chmod(runtime_mode)
    state.chmod(int(values["StateDirectoryMode"][0], 8))
    settings = [f"{name}={names.get(value, value)}" for name, value in environment.items()
                if name not in ("AGENTIC_RUNTIME_DIR", "AGENTIC_STATE_DIR")]
    sock = runtime / "flowerlyd.sock"
    log_path = root / f"shipped-{label}.log"
    with log_path.open("wb") as log:
        process = subprocess.Popen(
            ["runuser", "-u", service, "-g", group, "--", "env", "-i", "PATH=/usr/bin:/bin",
             f"HOME={state}", f"AGENTIC_RUNTIME_DIR={runtime}", f"AGENTIC_STATE_DIR={state}",
             *settings, "sh", "-c", f"umask {values['UMask'][0]} && exec \"$0\"",
             str(local_bin / "flowerlyd")],
            stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(200):
            if process.poll() is not None:
                raise AssertionError(f"flowerlyd exited early: {log_path.read_text()}")
            if sock.exists() and as_user(service, script, sock, {"type": "ping"})["outcome"] == "reply":
                break
            time.sleep(0.05)
        else:
            raise AssertionError(f"flowerlyd not ready: {log_path.read_text()}")
        meta = os.stat(sock)
        assert meta.st_mode & 0o777 == (0o660 if opt_in else 0o600), (label, oct(meta.st_mode))
        assert meta.st_gid == gid, (label, meta.st_gid, gid)
        assert os.stat(runtime).st_mode & 0o777 == runtime_mode, oct(os.stat(runtime).st_mode)
        # The database stays private to the service account in both.
        for entry in state.iterdir():
            mode = entry.stat().st_mode & 0o777
            assert mode & 0o077 == 0 and entry.stat().st_uid == service_pw.pw_uid, (
                label, entry, oct(mode))
        status = reply_of(as_user(service, script, sock, {"type": "status"}), "status")["data"]["details"]
        expected = "service_account_and_linux_groups" if opt_in else "service_account_only"
        assert status["client_access"] == expected, status
        assert status["admin_authorization"] == "linux_group", status

        if opt_in:
            # Users members connect for User commands only; the human admin
            # also runs Admin commands; neither gets a PTY.
            assert reply_of(as_user(users, script, sock, command("flow.list")), "u")["type"] == "ok"
            for name in ADMIN_COMMANDS + ["session.spawn"]:
                assert is_denied(reply_of(as_user(users, script, sock, command(name)), name)), name
            for name in ADMIN_COMMANDS:
                assert not is_denied(reply_of(as_user(admin, script, sock, command(name)), name)), name
            assert is_denied(reply_of(as_user(admin, script, sock, command("session.spawn")), "pty"))
            refused = (admin_only, mallory)
        else:
            refused = (users, admin, admin_only, mallory)
        # Everyone else cannot reach the socket...
        for user in refused:
            assert as_user(user, script, sock, command("flow.list"))["outcome"] == "eacces", (label, user)
        # ...and the kernel check refuses them even when the modes are opened.
        runtime.chmod(0o755)
        os.chmod(sock, 0o666)
        for user in refused:
            assert as_user(user, script, sock, command("flow.list"))["outcome"] == "closed", (label, user)
        os.chmod(sock, meta.st_mode & 0o777)
        runtime.chmod(runtime_mode)

        # The service account is not an administrator in either configuration.
        for name in ADMIN_COMMANDS:
            assert is_denied(reply_of(as_user(service, script, sock, command(name)), name)), name
        assert is_denied(reply_of(as_user(service, script, sock, {"type": "shutdown"}), "svc"))
        assert process.poll() is None, log_path.read_text()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=15)
    records = audit_records(log_path.read_text())
    for name in ADMIN_COMMANDS:
        assert (name, "denied", "true") in records, (label, name, records)
    print(f"shipped unit ({label}, #225): " + (
        "users members admitted for User commands only, human admin for Admin, no PTY; "
        "admin-only and non-members refused at filesystem and kernel check; state private; "
        "service account not admin: OK" if opt_in else
        "service-account-only at filesystem and kernel check; users members, admins and "
        "non-members refused; state private; service account not admin: OK"))


def reply_of(outcome, label):
    assert outcome["outcome"] == "reply", (label, outcome)
    return outcome["reply"]


def cross_uid(bin_dir):
    if os.geteuid() != 0:
        raise SystemExit("--cross-uid must run as root (CI uses sudo)")
    suffix = uuid.uuid4().hex[:6]
    users_group, admin_group = f"flw-users-{suffix}", f"flw-admin-{suffix}"
    service, alice, bob, mallory, carol = (
        f"flw-{name}-{suffix}" for name in ("svc", "alice", "bob", "mal", "aon"))
    created_users, created_groups = [], []
    root = pathlib.Path(tempfile.mkdtemp(prefix="flowerly-cross-uid-"))
    process = None
    try:
        for group in (users_group, admin_group):
            run("groupadd", "--system", group)
            created_groups.append(group)
        # bob is a human administrator (users + admin); carol is in the admin
        # group only, which never admits a connection by itself.
        for user, groups in [(service, None), (alice, users_group),
                             (bob, f"{users_group},{admin_group}"), (mallory, None),
                             (carol, admin_group)]:
            argv = ["useradd", "--system", "--user-group", "--no-create-home",
                    "--shell", "/usr/sbin/nologin"]
            if groups:
                argv += ["--groups", groups]
            run(*argv, user)
            created_users.append(user)
        service_pw = pwd.getpwnam(service)
        users_gid = grp.getgrnam(users_group).gr_gid

        root.chmod(0o755)
        local_bin = root / "bin"
        local_bin.mkdir(mode=0o755)
        shutil.copy2(bin_dir / "flowerlyd", local_bin / "flowerlyd")
        script = root / "test-admin-authorization.py"
        shutil.copy2(__file__, script)
        script.chmod(0o644)
        # Mirrors the systemd unit: runtime dir group-traversable by the
        # users group, state private to the service account.
        runtime, state = root / "runtime", root / "state"
        runtime.mkdir(mode=0o750)
        state.mkdir(mode=0o700)
        os.chown(runtime, service_pw.pw_uid, users_gid)
        os.chown(state, service_pw.pw_uid, service_pw.pw_gid)
        sock = runtime / "flowerlyd.sock"
        log_path = root / "flowerlyd.log"
        with log_path.open("wb") as log:
            process = subprocess.Popen(
                # Like systemd Group=: the users group is the primary group.
                ["runuser", "-u", service, "-g", users_group, "--",
                 "env", "-i", "PATH=/usr/bin:/bin", f"HOME={state}",
                 f"AGENTIC_RUNTIME_DIR={runtime}", f"AGENTIC_STATE_DIR={state}",
                 f"AGENTIC_USERS_GROUP={users_group}", f"AGENTIC_ADMIN_GROUP={admin_group}",
                 str(local_bin / "flowerlyd")],
                stdout=log, stderr=subprocess.STDOUT)
        for _ in range(200):
            if process.poll() is not None:
                raise AssertionError(f"flowerlyd exited early: {log_path.read_text()}")
            if sock.exists() and as_user(service, script, sock, {"type": "ping"})["outcome"] == "reply":
                break
            time.sleep(0.05)
        else:
            raise AssertionError(f"flowerlyd not ready: {log_path.read_text()}")
        meta = os.stat(sock)
        assert meta.st_mode & 0o777 == 0o660 and meta.st_gid == users_gid, (oct(meta.st_mode), meta.st_gid)

        status = reply_of(as_user(alice, script, sock, {"type": "status"}), "alice status")
        assert status["data"]["details"]["admin_authorization"] == "linux_group", status

        # Users-group member: User commands only.
        assert reply_of(as_user(alice, script, sock, command("flow.list")), "alice")["type"] == "ok"
        created = reply_of(as_user(alice, script, sock, command("goal.create", {"objective": "x"})), "alice")
        assert created["type"] == "ok", created
        # The owner reads it back; bob (another uid, even as an admin) cannot (#225).
        shown = reply_of(as_user(alice, script, sock, command("goal.show", {"id": created["data"]["id"]})), "show")
        hidden = reply_of(as_user(bob, script, sock, command("goal.show", {"id": created["data"]["id"]})), "bob show")
        assert hidden["type"] == "err" and hidden["code"] == "not_found", hidden
        # The kernel uid, not the spoofed "uid:0", is recorded as creator.
        assert shown["data"]["created_by"] == f"uid:{pwd.getpwnam(alice).pw_uid}", shown
        # Ownership is recorded from the kernel uid (#225), independent of the
        # spoofed JSON, and General is a system object. Read the database
        # directly as root: the state directory is private to the service.
        db = sqlite3.connect(f"file:{state / 'state.db'}?mode=ro", uri=True)
        try:
            owner = db.execute("SELECT owner_class, owner_uid FROM goals WHERE id = ?",
                               (created["data"]["id"],)).fetchone()
            assert owner == ("user", pwd.getpwnam(alice).pw_uid), owner
            general = db.execute("SELECT owner_class, owner_uid FROM flows WHERE name = 'General'").fetchone()
            assert general == ("system", None), general
            assert db.execute("SELECT COUNT(*) FROM goals WHERE owner_class = 'unresolved'").fetchone()[0] == 0
        finally:
            db.close()
        for name in ADMIN_COMMANDS + ["session.spawn", "session.status"]:
            assert is_denied(reply_of(as_user(alice, script, sock, command(name)), name)), ("alice", name)
        assert is_denied(reply_of(as_user(alice, script, sock, {"type": "shutdown"}), "alice"))

        # The service account is not implicitly an administrator.
        for name in ADMIN_COMMANDS:
            assert is_denied(reply_of(as_user(service, script, sock, command(name)), name)), ("svc", name)
        assert is_denied(reply_of(as_user(service, script, sock, {"type": "shutdown"}), "svc"))

        # Admin-group member: Admin commands, but never a service-account shell.
        for name in ADMIN_COMMANDS:
            assert not is_denied(reply_of(as_user(bob, script, sock, command(name)), name)), ("bob", name)
        assert is_denied(reply_of(as_user(bob, script, sock, command("session.spawn")), "bob spawn"))

        # Non-member and admin-only member: the filesystem refuses, and so does
        # the kernel check when the directory and socket modes are opened up.
        for user in (mallory, carol):
            assert as_user(user, script, sock, {"type": "ping"})["outcome"] == "eacces", user
        runtime.chmod(0o755)
        os.chmod(sock, 0o666)
        for user in (mallory, carol):
            assert as_user(user, script, sock, command("flow.list"))["outcome"] == "closed", user
        os.chmod(sock, 0o660)
        runtime.chmod(0o750)
        assert process.poll() is None, log_path.read_text()

        reply = reply_of(as_user(bob, script, sock, {"type": "shutdown"}), "bob shutdown")
        assert reply["type"] == "ok", reply
        process.wait(timeout=15)
        assert process.returncode == 0, log_path.read_text()
        assert "rejecting IPC peer outside the access policy" in log_path.read_text()
        # The Admin journal names the human administrator's own uid.
        bob_uid = str(pwd.getpwnam(bob).pw_uid)
        granted = [f for f in audit_fields(log_path.read_text())
                   if f.get("peer_uid") == bob_uid and f.get("outcome") != "denied"]
        assert granted and all(f.get("service_account") == "false" for f in granted), granted
        print("cross-UID admin authorization: users member limited to User commands, human "
              "admin admitted for Admin and shutdown and journaled by own uid, admin-only "
              "member and non-member refused by kernel credentials, service account never "
              "implicit admin, PTY limited to the service account: OK")
        process = None
        for opt_in in (False, True):
            shipped_unit(root, local_bin, script, service, alice, bob, carol, mallory,
                         users_group, admin_group, opt_in)
    finally:
        if process is not None and process.poll() is None:
            process.kill()
            process.wait()
        for user in created_users:
            subprocess.run(["userdel", user], check=False)
        for group in created_groups:
            subprocess.run(["groupdel", group], check=False)
        shutil.rmtree(root, ignore_errors=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("bin_dir", nargs="?")
    parser.add_argument("--cross-uid", action="store_true")
    parser.add_argument("--client", nargs=2, metavar=("SOCKET", "PAYLOAD"))
    args = parser.parse_args()
    if args.client:
        client_main(*args.client)
        return
    if not args.bin_dir:
        raise SystemExit("usage: test-admin-authorization.py [--cross-uid] target/release")
    bin_dir = pathlib.Path(args.bin_dir).resolve()
    if args.cross_uid:
        cross_uid(bin_dir)
    else:
        same_uid(bin_dir)


if __name__ == "__main__":
    main()

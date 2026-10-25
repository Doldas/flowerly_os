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
  * an unknown group name is a startup error, not a silent downgrade.

--cross-uid (root, used by CI through sudo) creates a dedicated service
account plus users/admin groups and members, runs flowerlyd as the service
account and connects as each identity:
  * a users-group member runs User commands but no Admin, PTY or shutdown;
  * an admin-group member runs Admin commands and shutdown, but no PTY;
  * the service account is never implicitly admin;
  * a non-member is refused by the kernel credential check even when the
    socket file is made world-accessible.
It then starts flowerlyd exactly as the shipped systemd unit configures it
(#225 temporary default) and proves service-account-only admission at both
the filesystem and the kernel check, no implicit admin for the service
account, and the documented root-only out-of-band maintenance procedure.
"""
import argparse
import grp
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
    "model.register",
    "model.retire",
    "provider.register",
    "usage.record",
]
# Forged identity claims that must never influence authorization.
SPOOFED = {"admin": True, "created_by": "uid:0", "uid": 0, "groups": [0], "role": "admin"}


# Test-only acknowledgment that lets flowerlyd admit a users group before
# per-user ownership is enforced (#225); the shipped unit unsets it.
UNENFORCED_ACK = "AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP"
UNENFORCED_ACK_VALUE = "members-can-read-each-others-data"


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


def audit_records(log_text):
    """(command, outcome, service_account) of each `admin command` journal record."""
    records = []
    for line in ANSI.sub("", log_text).splitlines():
        if "admin command" not in line:
            continue
        fields = dict(re.findall(r'(\w+)=("[^"]*"|\S+)', line))
        records.append((fields.get("command", "").strip('"'),
                        fields.get("outcome", "").strip('"'),
                        fields.get("service_account")))
    return records


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
    env.pop(UNENFORCED_ACK, None)
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

        # Until #225 enforces ownership, a users group refuses to start unless
        # the exact test-only acknowledgment is given.
        for label, extra in [
            ("users-no-ack", {}),
            ("users-wrong-ack", {UNENFORCED_ACK: "1"}),
        ]:
            process, sock, log = start(bin_dir, root, label,
                                       {"AGENTIC_USERS_GROUP": str(os.getgid()), **extra})
            try:
                process.wait(timeout=15)
            finally:
                stop(process)
            assert process.returncode != 0, f"{label}: a users group must refuse to start"
            assert "#225" in log.read_text() and not sock.exists(), log.read_text()
        print("same-UID admin authorization: Admin commands and shutdown refused without an "
              "explicit admin group; spoofed JSON ignored; explicit grant admitted; "
              "unknown group refused at startup: OK")


def run(*argv):
    subprocess.run(argv, check=True, stdout=subprocess.DEVNULL)


def as_user(user, script, sock, payload, groups=()):
    result = subprocess.run(
        ["runuser", "-u", user, *groups, "--", "env", "-i", "PATH=/usr/bin:/bin",
         sys.executable, "-I", str(script), "--client", str(sock), json.dumps(payload)],
        check=True, capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout)


UNIT = pathlib.Path(__file__).resolve().parents[1] / "distro/overlay/usr/lib/systemd/system/flowerlyd.service"


def unit_settings(path):
    """Active Environment= variables and other directives of a unit."""
    environment, values = {}, {}
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith(("#", ";", "[")) or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key == "Environment":
            name, setting = value.split("=", 1)
            environment[name] = setting
        else:
            values[key] = value
    return environment, values


def shipped_unit(root, local_bin, script, service, users, admin, users_group, admin_group):
    """#225: run flowerlyd exactly as the shipped unit configures it, with the
    unit's group names mapped to this run's test groups, and prove that only
    the service account is admitted, at the filesystem and the kernel check."""
    environment, values = unit_settings(UNIT)
    names = {"flowerly-users": users_group, "flowerly-admin": admin_group, "flowerly": service}
    service_pw = pwd.getpwnam(service)
    group = names[values["Group"]]
    gid = grp.getgrnam(group).gr_gid
    runtime, state = root / "shipped-runtime", root / "shipped-state"
    runtime.mkdir()
    state.mkdir()
    os.chown(runtime, service_pw.pw_uid, gid)
    os.chown(state, service_pw.pw_uid, gid)
    runtime.chmod(int(values["RuntimeDirectoryMode"], 8))
    state.chmod(int(values["StateDirectoryMode"], 8))
    settings = [f"{name}={names.get(value, value)}" for name, value in environment.items()
                if name not in ("AGENTIC_RUNTIME_DIR", "AGENTIC_STATE_DIR")]
    sock = runtime / "flowerlyd.sock"
    log_path = root / "shipped.log"
    with log_path.open("wb") as log:
        process = subprocess.Popen(
            ["runuser", "-u", service, "-g", group, "--", "env", "-i", "PATH=/usr/bin:/bin",
             f"HOME={state}", f"AGENTIC_RUNTIME_DIR={runtime}", f"AGENTIC_STATE_DIR={state}",
             *settings, str(local_bin / "flowerlyd")],
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
        assert os.stat(sock).st_mode & 0o777 == 0o600, oct(os.stat(sock).st_mode)
        assert os.stat(runtime).st_mode & 0o777 == 0o700, oct(os.stat(runtime).st_mode)
        status = reply_of(as_user(service, script, sock, {"type": "status"}), "status")["data"]["details"]
        assert status["client_access"] == "service_account_only", status
        assert status["admin_authorization"] == "linux_group", status

        # A users member and a different-UID admin cannot reach the socket...
        for user in (users, admin):
            assert as_user(user, script, sock, command("flow.list"))["outcome"] == "eacces", user
        # ...and the kernel check refuses them even when the modes are opened.
        runtime.chmod(0o755)
        os.chmod(sock, 0o666)
        for user in (users, admin):
            assert as_user(user, script, sock, command("flow.list"))["outcome"] == "closed", user
        os.chmod(sock, 0o600)
        runtime.chmod(0o700)

        # The service account is not an administrator by default.
        for name in ADMIN_COMMANDS:
            assert is_denied(reply_of(as_user(service, script, sock, command(name)), name)), name
        assert is_denied(reply_of(as_user(service, script, sock, {"type": "shutdown"}), "svc"))

        # Documented out-of-band maintenance: root runs ONE client process as
        # the service account with the admin group added to that process only.
        maintenance = ("-g", group, "-G", admin_group)
        for name in ADMIN_COMMANDS:
            reply = reply_of(as_user(service, script, sock, command(name), maintenance), name)
            assert not is_denied(reply), (name, reply)
        reply = reply_of(as_user(service, script, sock, {"type": "shutdown"}, maintenance), "stop")
        assert reply["type"] == "ok", reply
        process.wait(timeout=15)
        assert process.returncode == 0, log_path.read_text()
        # The journal shows the service account, not the human operator: the
        # operator must be identified from the sudo record (documented).
        records = audit_records(log_path.read_text())
        assert ("shutdown", "accepted", "true") in records, records
        for name in ADMIN_COMMANDS:
            assert (name, "denied", "true") in records, (name, records)
            assert any(c == name and o in ("succeeded", "failed") for c, o, _ in records), name
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
    print("shipped unit (#225): service-account-only at filesystem and kernel check; "
          "users members and other-UID admins refused; service account not admin; "
          "root-only out-of-band admin maintenance works: OK")


def reply_of(outcome, label):
    assert outcome["outcome"] == "reply", (label, outcome)
    return outcome["reply"]


def cross_uid(bin_dir):
    if os.geteuid() != 0:
        raise SystemExit("--cross-uid must run as root (CI uses sudo)")
    suffix = uuid.uuid4().hex[:6]
    users_group, admin_group = f"flw-users-{suffix}", f"flw-admin-{suffix}"
    service, alice, bob, mallory = (f"flw-{name}-{suffix}" for name in ("svc", "alice", "bob", "mal"))
    created_users, created_groups = [], []
    root = pathlib.Path(tempfile.mkdtemp(prefix="flowerly-cross-uid-"))
    process = None
    try:
        for group in (users_group, admin_group):
            run("groupadd", "--system", group)
            created_groups.append(group)
        for user, groups in [(service, None), (alice, users_group),
                             (bob, f"{users_group},{admin_group}"), (mallory, None)]:
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
                 f"{UNENFORCED_ACK}={UNENFORCED_ACK_VALUE}",
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

        # Non-member: the filesystem refuses, and so does the kernel check
        # when the directory and socket modes are deliberately opened up.
        assert as_user(mallory, script, sock, {"type": "ping"})["outcome"] == "eacces"
        runtime.chmod(0o755)
        os.chmod(sock, 0o666)
        assert as_user(mallory, script, sock, command("flow.list"))["outcome"] == "closed"
        os.chmod(sock, 0o660)
        runtime.chmod(0o750)
        assert process.poll() is None, log_path.read_text()

        reply = reply_of(as_user(bob, script, sock, {"type": "shutdown"}), "bob shutdown")
        assert reply["type"] == "ok", reply
        process.wait(timeout=15)
        assert process.returncode == 0, log_path.read_text()
        assert "rejecting IPC peer outside the access policy" in log_path.read_text()
        print("cross-UID admin authorization: users member limited to User commands, admin "
              "member admitted for Admin and shutdown, service account never implicit admin, "
              "PTY limited to the service account, non-member refused by kernel credentials: OK")
        process = None
        shipped_unit(root, local_bin, script, service, alice, bob, users_group, admin_group)
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

#!/usr/bin/env python3
"""#225 acceptance: per-user data ownership with two real Linux UIDs.

--cross-uid (root, used by CI through sudo) creates a service account, a
users group and two members, alice and bob, and runs the real flowerlyd as
the service account with multi-user admission. This needs the test-only
acknowledgment, because shipped flowerlyd refuses a users group until #225
closes. It then turns the two-UID reproduction from #225 into permanent
assertions:

  * Status counts and the foreground are the caller's own. The status-count
    leak is fixed.
  * flow.list shows only the caller's Flows plus General, each with the
    caller's own `foreground` flag.
  * Another uid's Flow, Goal, Session or Execution is answered exactly like
    a missing one (`not_found`, same message). That holds for show, list,
    activate, archive, complete, abandon, child, terminate and for
    cross-object references (Session in another uid's Flow, Execution or
    child Goal of another uid's Goal). Spoofed JSON identity fields do not
    help.
  * Activating any Flow, including General, never changes another uid's
    foreground. The foreground-mutation bug is fixed.
  * Provenance edges are shown only when both endpoints are visible, and
    another uid's object answers like a missing reference. A Generation
    may name only its creator's own Goal, and inspect never reveals another
    uid's Goal. No Status count changes for bob when alice adds data.
  * Foregrounds survive a daemon restart, and the database holds one
    flow_foreground row per uid that chose a non-General Flow.
  * Compute (PR 5): profiles are private with names unique per owner (no
    name-existence oracle); a separate admin user registers a model and
    records usage through Admin commands. Workloads may use only the
    caller's own profile; another uid's workload, its usage totals and its
    provenance are answered like missing ones; `workload.list` never lists
    other users' workloads.
  * Generations (PR 6): candidates are private until the admin promotes
    one (audited, Admin only). Promotion shares the Generation, not its
    reason Goal. A private candidate cannot be activated, and no User
    command is pending any more.

Default mode (no root) runs only the offline parser and helper self-checks.
"""
import argparse
import grp
import json
import os
import pathlib
import pwd
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import uuid

UNENFORCED_ACK = "AGENTIC_TEST_ONLY_UNENFORCED_OWNERSHIP"
UNENFORCED_ACK_VALUE = "members-can-read-each-others-data"
MISSING_FLOW = "flow-00000000000000000000000000000000"
MISSING_GOAL = "goal-00000000000000000000000000000000"
MISSING_PROFILE = "profile-00000000000000000000000000000000"
MISSING_WORKLOAD = "work-00000000000000000000000000000000"
MISSING_GENERATION = "gen-00000000000000000000000000000000"


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


def client_main(socket_path, payload):
    """One request as the invoking identity; prints a JSON outcome."""
    try:
        print(json.dumps({"outcome": "reply", "reply": rpc(socket_path, json.loads(payload))}))
    except PermissionError:
        print(json.dumps({"outcome": "eacces"}))
    except (ConnectionResetError, BrokenPipeError):
        print(json.dumps({"outcome": "closed"}))


def run(*argv):
    subprocess.run(argv, check=True, stdout=subprocess.DEVNULL)


class Peer:
    """Sends requests to flowerlyd as one real Linux user."""

    def __init__(self, user, script, sock, spoof_uid):
        self.user, self.script, self.sock = user, script, sock
        # Forged identity claims naming the *other* user must change nothing.
        self.spoof = {"created_by": f"uid:{spoof_uid}", "uid": spoof_uid, "owner_uid": spoof_uid}

    def send(self, payload):
        result = subprocess.run(
            ["runuser", "-u", self.user, "--", "env", "-i", "PATH=/usr/bin:/bin",
             sys.executable, "-I", str(self.script), "--client", str(self.sock),
             json.dumps(payload)],
            check=True, capture_output=True, text=True, timeout=30)
        outcome = json.loads(result.stdout)
        assert outcome["outcome"] == "reply", (self.user, payload, outcome)
        return outcome["reply"]

    def call(self, command, /, **args):
        return self.send({"type": "command", "name": command, "args": dict(self.spoof, **args)})

    def ok(self, command, /, **args):
        reply = self.call(command, **args)
        assert reply["type"] == "ok", (self.user, command, args, reply)
        return reply["data"]

    def status(self):
        reply = self.send({"type": "status"})
        assert reply["type"] == "ok", reply
        return reply["data"]["details"]


def error_of(reply):
    assert reply["type"] == "err", reply
    return reply["code"], reply["message"]


def assert_like_missing(peer, name, hidden_id, missing_id, /, **args):
    """Another uid's object must be indistinguishable from a missing one."""
    hidden = error_of(peer.call(name, **dict(args, id=hidden_id)))
    missing = error_of(peer.call(name, **dict(args, id=missing_id)))
    assert hidden[0] == missing[0] == "not_found", (name, hidden, missing)
    assert hidden[1].replace(hidden_id, "<id>") == missing[1].replace(missing_id, "<id>"), (
        name, hidden, missing)


def counts(details):
    return {key: details[key] for key in
            ("flow_count", "goal_count", "session_count", "execution_count")}


def flows(peer):
    return {(flow["name"], flow["foreground"]) for flow in peer.ok("flow.list")["flows"]}


def ping_as(user, script, sock):
    result = subprocess.run(
        ["runuser", "-u", user, "--", "env", "-i", "PATH=/usr/bin:/bin", sys.executable, "-I",
         str(script), "--client", str(sock), json.dumps({"type": "ping"})],
        capture_output=True, text=True, timeout=30)
    return result.returncode == 0 and json.loads(result.stdout or "{}").get("outcome") == "reply"


def start_daemon(script, local_bin, service, users_group, admin_group, runtime, state,
                 log_path):
    with log_path.open("ab") as log:
        process = subprocess.Popen(
            ["runuser", "-u", service, "-g", users_group, "--",
             "env", "-i", "PATH=/usr/bin:/bin", f"HOME={state}",
             f"AGENTIC_RUNTIME_DIR={runtime}", f"AGENTIC_STATE_DIR={state}",
             f"AGENTIC_USERS_GROUP={users_group}", f"AGENTIC_ADMIN_GROUP={admin_group}",
             f"{UNENFORCED_ACK}={UNENFORCED_ACK_VALUE}",
             str(local_bin / "flowerlyd")],
            stdout=log, stderr=subprocess.STDOUT)
    sock = runtime / "flowerlyd.sock"
    for _ in range(200):
        if process.poll() is not None:
            raise AssertionError(f"flowerlyd exited early: {log_path.read_text()}")
        # Root is not admitted; readiness is checked as the service account.
        if sock.exists() and ping_as(service, script, sock):
            return process, sock
        time.sleep(0.05)
    raise AssertionError(f"flowerlyd not ready: {log_path.read_text()}")


def stop_daemon(process):
    if process.poll() is None:
        process.terminate()
        process.wait(timeout=15)


def ownership_scenario(alice, bob, alice_uid, bob_uid, db_path, restart):
    general = {("General", True)}
    # Alice builds a private graph and selects her own Flow.
    flow_a = alice.ok("flow.create", name="Alice private")["id"]
    created_goal = alice.ok("goal.create", objective="alice secret")
    goal_a, goal_status = created_goal["id"], created_goal["status"]
    session_a = alice.ok("session.create", flow_id=flow_a, kind="metadata")["id"]
    exec_a = alice.ok("execution.create", goal_id=goal_a, command="true")["id"]
    # The status-count leak reproduced on #225: bob counted alice's objects.
    assert counts(bob.status()) == {"flow_count": 1, "goal_count": 0, "session_count": 0,
                                    "execution_count": 0}, bob.status()
    activated = alice.ok("flow.activate", id=flow_a)
    assert activated.get("foreground") is True and activated["state"] == "ACTIVE", activated

    # Foreground: each uid has its own, and General is bob's fallback.
    assert bob.status()["foreground_flow"]["name"] == "General"
    assert counts(alice.status()) == {"flow_count": 2, "goal_count": 1, "session_count": 1,
                                      "execution_count": 1}, alice.status()
    assert alice.status()["foreground_flow"]["id"] == flow_a
    assert flows(bob) == general, flows(bob)
    assert flows(alice) == {("General", False), ("Alice private", True)}, flows(alice)

    # Another uid's objects are answered exactly like missing ones.
    for name in ("flow.activate", "flow.archive"):
        assert_like_missing(bob, name, flow_a, MISSING_FLOW)
    assert error_of(bob.call("flow.activate", name="Alice private"))[0] == "not_found"
    for name in ("goal.show", "goal.complete", "goal.abandon"):
        assert_like_missing(bob, name, goal_a, MISSING_GOAL)
    assert_like_missing(bob, "goal.child", goal_a, MISSING_GOAL, objective="graft")
    assert_like_missing(bob, "session.terminate", session_a,
                        "session-00000000000000000000000000000000")
    assert bob.ok("goal.list")["count"] == 0
    assert bob.ok("session.list")["count"] == 0
    assert bob.ok("execution.list")["count"] == 0
    assert bob.ok("execution.show", id=exec_a)["execution"] is None

    # Cross-object references to alice's objects are refused the same way.
    for name, args in [("session.create", {"flow_id": flow_a, "kind": "metadata"}),
                       ("execution.create", {"goal_id": goal_a})]:
        code, message = error_of(bob.call(name, **args))
        assert code == "not_found", (name, code, message)

    # General is shared and protected; bob can use it but not archive it.
    general_id = next(flow["id"] for flow in bob.ok("flow.list")["flows"])
    bob.ok("session.create", flow_id=general_id, kind="metadata")
    assert error_of(bob.call("flow.archive", id=general_id))[0] == "permission_denied"

    # Foreground mutation: nothing bob activates changes alice's foreground.
    flow_b = bob.ok("flow.create", name="Bob private")["id"]
    bob.ok("flow.activate", id=flow_b)
    assert alice.status()["foreground_flow"]["id"] == flow_a
    bob.ok("flow.activate", id=general_id)
    assert bob.status()["foreground_flow"]["name"] == "General"
    assert alice.status()["foreground_flow"]["id"] == flow_a
    bob.ok("flow.activate", name="Bob private")

    # Alice's objects are unchanged by everything bob tried.
    assert alice.ok("goal.show", id=goal_a)["status"] == goal_status
    assert alice.ok("goal.list")["count"] == 1
    assert alice.ok("session.list")["sessions"][0]["state"] == "RUNNING"
    assert alice.ok("execution.list")["count"] == 1

    # Provenance is filtered by both endpoints (PR 4), like a missing ref.
    for name, ref in [("provenance.from", exec_a), ("provenance.to", goal_a),
                      ("provenance.to", f"actor:uid:{alice_uid}"),
                      ("provenance.from", MISSING_GOAL)]:
        assert bob.ok(name, ref=ref)["count"] == 0, (name, ref)
    assert alice.ok("provenance.to", ref=goal_a)["count"] >= 1
    assert alice.ok("provenance.to", ref=f"actor:uid:{alice_uid}")["count"] >= 1

    # Generation reason Goals: only one's own, and never revealed to others.
    code, _ = error_of(bob.call("generation.create", reason_goal=goal_a))
    assert code == "not_found", code
    bob_view = bob.status()
    generation = alice.ok("generation.create", reason_goal=goal_a)["id"]
    assert alice.ok("generation.inspect", id=generation)["reason_goal"] == goal_a
    # Candidates are private (PR 6); see generation_scenario for promotion.
    assert_like_missing(bob, "generation.inspect", generation, MISSING_GENERATION)

    # Nothing alice did changes any count bob sees.
    alice.ok("goal.create", objective="more private work")
    after = bob.status()
    for key in ("flow_count", "goal_count", "session_count", "execution_count",
                "generation_count", "provenance_edge_count", "profile_count",
                "active_workload_count"):
        assert after[key] == bob_view[key], (key, bob_view[key], after[key])

    # Restart: each uid keeps its own foreground.
    restart()
    assert alice.status()["foreground_flow"]["id"] == flow_a
    assert bob.status()["foreground_flow"]["id"] == flow_b
    alice.ok("flow.archive", id=flow_a)
    assert alice.status()["foreground_flow"]["name"] == "General"
    assert bob.status()["foreground_flow"]["id"] == flow_b

    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = db.execute("SELECT owner_uid, flow_id FROM flow_foreground").fetchall()
        assert rows == [(bob_uid, flow_b)], rows
        assert db.execute("SELECT COUNT(*) FROM flows WHERE state = 'FOREGROUND'").fetchone()[0] == 0
        owners = dict(db.execute("SELECT id, owner_uid FROM flows WHERE owner_class = 'user'"))
        assert owners == {flow_a: alice_uid, flow_b: bob_uid}, owners
    finally:
        db.close()


def compute_scenario(alice, bob, admin):
    bob_view = bob.status()
    model = admin.ok("model.register", name="tiny-cpu", backend="CPU",
                     measured_memory_bytes=1_000_000, context_tokens=2048,
                     modalities=["text"], license="apache-2.0", evidence="measure:test")["id"]

    # Profiles: private, and the same name is free for another owner.
    profile_a = alice.ok("profile.create", name="expert", toml="fallback = 'queue'\n")["id"]
    profile_b = bob.ok("profile.create", name="expert", toml="fallback = 'ask'\n")["id"]
    assert error_of(alice.call("profile.create", name="expert",
                               toml="fallback = 'ask'\n"))[0] == "conflict"
    assert [p["id"] for p in bob.ok("profile.list")["profiles"]] == [profile_b]
    for name, args in [("profile.show", {}), ("profile.export", {}),
                       ("profile.version_history", {}), ("profile.resolve", {}),
                       ("profile.clone", {"name": "copy"}),
                       ("profile.rename", {"name": "taken"}),
                       ("profile.edit", {"toml": "fallback = 'fail'\n"})]:
        assert_like_missing(bob, name, profile_a, MISSING_PROFILE, **args)
    code, _ = error_of(bob.call("profile.create", name="graft", toml="base = 'derived'\n"))
    assert code == "not_found", code

    # Workloads: only one's own profile, and private like missing ones.
    work_a = alice.ok("workload.submit", profile_id=profile_a, model_id=model)["id"]
    work_b = bob.ok("workload.submit", profile_id=profile_b, model_id=model)["id"]
    code, _ = error_of(bob.call("workload.submit", profile_id=profile_a, model_id=model))
    assert code == "not_found", code
    for name in ("workload.show", "workload.stop", "workload.complete"):
        assert_like_missing(bob, name, work_a, MISSING_WORKLOAD)
    code, message = error_of(bob.call("inference.request", workload_id=work_a, prompt="hi",
                                      request_id="r-1", provider_id="provider-" + "0" * 32))
    assert code == "not_found" and "workload" in message, (code, message)
    assert [w["id"] for w in bob.ok("workload.list")["workloads"]] == [work_b]
    assert error_of(bob.call("workload.list", mine=False))[0] == "invalid_argument"
    assert bob.ok("provenance.from", ref=work_a)["count"] == 0
    assert alice.ok("provenance.from", ref=work_a)["count"] >= 1

    # Usage recorded by the admin for alice's workload is alice's alone.
    admin.ok("usage.record", workload_id=work_a, tokens_in=7, tokens_out=11, latency_ms=3,
             cost_micros=5)
    assert bob.ok("workload.usage_brief", model_id=model)["tokens_in"] == 0
    assert bob.ok("workload.show", id=work_b)["usage_tokens_in_total_for_model"] == 0
    assert alice.ok("workload.usage_brief", model_id=model)["tokens_in"] == 7

    # Bob's own profile and workload are his only visible changes.
    after = bob.status()
    assert after["profile_count"] == bob_view["profile_count"] + 1, (bob_view, after)
    for key in ("goal_count", "session_count", "execution_count", "generation_count"):
        assert after[key] == bob_view[key], (key, bob_view[key], after[key])


def generation_scenario(alice, bob, admin, admin_uid):
    goal = alice.ok("goal.create", objective="why this image")["id"]
    candidate = alice.ok("generation.create", reason_goal=goal, source_commit="abc")["id"]
    visible = {g["id"] for g in bob.ok("generation.list")["generations"]}
    assert candidate not in visible, visible
    assert_like_missing(bob, "generation.inspect", candidate, MISSING_GENERATION)
    code, _ = error_of(bob.call("generation.create", parent=candidate))
    assert code == "not_found", code
    # A private candidate can never become the system STABLE directly.
    assert error_of(admin.call("generation.activate", id=candidate))[0] == "conflict"
    assert error_of(admin.call("generation.promote", id=candidate, reason=" "))[0] == (
        "invalid_argument")
    # Promotion is Admin only, then shares the Generation but not its reason Goal.
    assert error_of(bob.call("generation.promote", id=candidate,
                             reason="mine now"))[0] == "permission_denied"
    promoted = admin.ok("generation.promote", id=candidate, reason="ship the fix")
    assert promoted["owner"] == "system", promoted
    shown = bob.ok("generation.inspect", id=candidate)
    assert shown["source_commit"] == "abc" and shown["reason_goal"] is None, shown
    assert alice.ok("generation.inspect", id=candidate)["reason_goal"] == goal
    edges = {edge["to"] for edge in bob.ok("provenance.from", ref=candidate)["edges"]}
    assert goal not in edges and f"actor:uid:{admin_uid}" not in edges, edges
    bob.ok("generation.create", parent=candidate)
    admin.ok("generation.activate", id=candidate)
    assert bob.ok("generation.inspect", id=candidate)["status"] == "STABLE"
    assert alice.status()["ownership_pending_commands"] == 0


def cross_uid(bin_dir):
    if os.geteuid() != 0:
        raise SystemExit("--cross-uid must run as root (CI uses sudo)")
    suffix = uuid.uuid4().hex[:6]
    users_group, admin_group = f"flw-own-{suffix}", f"flw-adg-{suffix}"
    service, alice, bob, admin = (f"flw-{name}-{suffix}" for name in ("svc", "ali", "bob", "adm"))
    created_users, created_groups = [], []
    root = pathlib.Path(tempfile.mkdtemp(prefix="flowerly-ownership-"))
    process = None
    try:
        for group in (users_group, admin_group):
            run("groupadd", "--system", group)
            created_groups.append(group)
        for user, groups in [(service, None), (alice, users_group), (bob, users_group),
                             (admin, f"{users_group},{admin_group}")]:
            argv = ["useradd", "--system", "--user-group", "--no-create-home",
                    "--shell", "/usr/sbin/nologin"]
            if groups:
                argv += ["--groups", groups]
            run(*argv, user)
            created_users.append(user)
        service_pw = pwd.getpwnam(service)
        users_gid = grp.getgrnam(users_group).gr_gid
        alice_uid, bob_uid = pwd.getpwnam(alice).pw_uid, pwd.getpwnam(bob).pw_uid

        root.chmod(0o755)
        local_bin = root / "bin"
        local_bin.mkdir(mode=0o755)
        shutil.copy2(bin_dir / "flowerlyd", local_bin / "flowerlyd")
        script = root / "test-user-ownership.py"
        shutil.copy2(__file__, script)
        script.chmod(0o644)
        runtime, state = root / "runtime", root / "state"
        runtime.mkdir(mode=0o750)
        state.mkdir(mode=0o700)
        os.chown(runtime, service_pw.pw_uid, users_gid)
        os.chown(state, service_pw.pw_uid, service_pw.pw_gid)
        log_path = root / "flowerlyd.log"
        process, sock = start_daemon(script, local_bin, service, users_group, admin_group,
                                     runtime, state, log_path)
        assert "TEST ONLY" in log_path.read_text(), log_path.read_text()

        def restart():
            nonlocal process
            stop_daemon(process)
            process, _ = start_daemon(script, local_bin, service, users_group, admin_group,
                                      runtime, state, log_path)

        alice_peer = Peer(alice, script, sock, bob_uid)
        bob_peer = Peer(bob, script, sock, alice_uid)
        ownership_scenario(alice_peer, bob_peer, alice_uid, bob_uid, state / "state.db", restart)
        admin_peer = Peer(admin, script, sock, alice_uid)
        compute_scenario(alice_peer, bob_peer, admin_peer)
        generation_scenario(alice_peer, bob_peer, admin_peer, pwd.getpwnam(admin).pw_uid)
        assert process.poll() is None, log_path.read_text()
        print("cross-UID ownership (#225): status counts and foreground are per caller; "
              "other users' Flows, Goals, Sessions and Executions are not_found like missing "
              "ones, including cross-object references; provenance needs both endpoints "
              "visible; Generation reason Goals stay private; General is shared and "
              "protected; no activation changes another user's foreground; foregrounds "
              "survive restart; profiles, workloads and usage are private; Generation "
              "candidates are private until an audited Admin promotion, which never "
              "reveals the reason Goal: OK")
    finally:
        if process is not None:
            stop_daemon(process)
        for user in created_users:
            subprocess.run(["userdel", user], check=False)
        for group in created_groups:
            subprocess.run(["groupdel", group], check=False)
        shutil.rmtree(root, ignore_errors=True)


def self_check():
    """Offline checks of the helpers, run by the docs preflight."""
    assert counts({"flow_count": 1, "goal_count": 2, "session_count": 3,
                   "execution_count": 4, "model_count": 9}) == {
        "flow_count": 1, "goal_count": 2, "session_count": 3, "execution_count": 4}
    assert error_of({"type": "err", "code": "not_found", "message": "m"}) == ("not_found", "m")
    try:
        error_of({"type": "ok", "data": {}})
    except AssertionError:
        pass
    else:
        raise AssertionError("an ok reply must not pass as an error")
    print("test-user-ownership.py self-check: OK (run --cross-uid as root for acceptance)")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("bin_dir", nargs="?")
    parser.add_argument("--cross-uid", action="store_true")
    parser.add_argument("--client", nargs=2, metavar=("SOCKET", "PAYLOAD"))
    args = parser.parse_args()
    if args.client:
        client_main(*args.client)
    elif args.cross_uid:
        if not args.bin_dir:
            raise SystemExit("usage: test-user-ownership.py --cross-uid target/release")
        cross_uid(pathlib.Path(args.bin_dir).resolve())
    else:
        self_check()


if __name__ == "__main__":
    main()

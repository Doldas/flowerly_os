#!/usr/bin/env python3
"""#72 adversarial PTY containment for the real flowerlyd with delegated cgroup v2.

Runs as the PTY manager account inside a Delegate=yes unit, with the shell
under a distinct UID (arch-pty-cgroup-acceptance.sh --system ... BIN
containment). Every assertion is a kernel observation.

  1. Namespace escape: from the shell, write to the parent, management and
     host cgroup.procs, also from a new user namespace, a new user+mount+
     cgroup namespace with its own cgroup2 mount, and setns() into PID 1's
     namespaces. The shell must stay in its Session cgroup, and terminate
     must kill processes inside user namespaces too.
  2. Double-close and PID reuse: concurrent double terminate and double
     detach; a decoy process born with the exited shell's exact PID (via the
     root-side pty-pid-reuse-helper.py) survives stale terminate, the
     reconcile loop, flowerlyd SIGKILL and restart recovery.
  3. Hostile resource exhaustion: a fork bomb stops at pids.max, a memory
     hog is OOM-killed inside its cgroup, CPU spinners are throttled and an
     output flood stays bounded; flowerlyd stays responsive and another
     Session keeps working throughout.
  4. Zombies and orphans: double-forked orphans, setsid orphans, unreaped
     zombies and a background job left by a shell that exits naturally are
     all gone after terminate; no zombie guardian remains.
  5. Fail closed: when the delegated root loses the pids controller a spawn
     is refused before any shell starts and leaves no cgroup behind.
"""
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import stat
import sys
import tempfile
import time
import uuid

spec = importlib.util.spec_from_file_location(
    "recovery", Path(__file__).with_name("test-pty-daemon-recovery.py"))
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)
ok, request, identity, alive, wait_gone, children = (
    recovery.ok, recovery.request, recovery.identity, recovery.alive,
    recovery.wait_gone, recovery.children)

SHELL_UID = int(os.environ.get("FLOWERLY_PTY_SHELL_UID", "0"))


def spawn(daemon):
    spawned = ok(daemon.socket, "session.spawn", {"shell": "/bin/sh"})
    spawned["writer"] = ok(daemon.socket, "session.attach", {
        "id": spawned["id"], "control_token": spawned["control_token"]})["attachment_token"]
    return spawned


def shell(daemon, spawned, command, marker=None, seconds=30):
    """Run `command` in the PTY shell; return output after `marker`."""
    marker = marker or f"FLW{uuid.uuid4().hex[:12]}"
    # A newline, not ";": `job & ; echo` is a shell syntax error.
    for line in (command + f"\necho {marker}:$?\n").encode().splitlines(keepends=True):
        while line:
            reply = request(daemon.socket, "session.write", {
                "id": spawned["id"], "attachment_token": spawned["writer"],
                "data": line[:4000].decode()})
            assert reply["type"] == "ok", reply
            line = line[4000:]
    deadline = time.monotonic() + seconds
    pattern = re.compile(marker.encode() + rb":(\d+)\r?\n")
    while time.monotonic() < deadline:
        data = recovery.base64.b64decode(ok(daemon.socket, "session.read", {
            "id": spawned["id"], "cursor": 0,
            "control_token": spawned["control_token"]})["output_base64"])
        found = list(pattern.finditer(data))
        if found:
            return data[:found[-1].start()].decode(errors="replace"), int(found[-1].group(1))
        time.sleep(0.05)
    raise AssertionError(f"shell command did not finish: {command[:80]}")


def terminate(daemon, spawned):
    return request(daemon.socket, "session.terminate", {
        "id": spawned["id"], "control_token": spawned["control_token"]})


def group_of(spawned):
    return f"pty-{spawned['id']}"


def members(daemon, spawned):
    path = daemon.root / group_of(spawned) / "cgroup.procs"
    try:
        return {int(pid) for pid in path.read_text().split()}
    except FileNotFoundError:
        return set()


def uid_processes(uid):
    found = {}
    for entry in Path("/proc").iterdir():
        if entry.name.isdigit():
            try:
                if entry.stat().st_uid == uid:
                    found[int(entry.name)] = identity(int(entry.name))
            except FileNotFoundError:
                pass
    return {pid: original for pid, original in found.items() if original}


def responsive(daemon, budget=1.5):
    started = time.monotonic()
    ok(daemon.socket, "session.list", {})
    elapsed = time.monotonic() - started
    assert elapsed < budget, f"flowerlyd took {elapsed:.2f}s to answer under load"
    return elapsed


def assert_session_gone(daemon, spawned, label):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and (daemon.root / group_of(spawned)).exists():
        time.sleep(0.02)
    assert not (daemon.root / group_of(spawned)).exists(), (label, "cgroup left")
    # Shell-UID processes may only live inside a live Session's cgroup.
    live = daemon.pty_groups()
    leftovers = {}
    for pid, original in uid_processes(SHELL_UID).items():
        try:
            group = Path(f"/proc/{pid}/cgroup").read_text().strip().rsplit("/", 1)[-1]
        except FileNotFoundError:
            continue
        if original[0] not in "ZX" and group not in live:
            leftovers[pid] = original
    if leftovers:
        detail = {}
        for pid in leftovers:
            try:
                detail[pid] = (Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ")
                               .decode(errors="replace")[:120],
                               Path(f"/proc/{pid}/cgroup").read_text().strip())
            except FileNotFoundError:
                detail[pid] = "exited"
        raise AssertionError((label, "shell-UID processes survived", detail))


def case_namespace_escape(daemon, tools):
    spawned = spawn(daemon)
    marker = f"PROBE{uuid.uuid4().hex[:10]}"
    output, status = shell(daemon, spawned, f"python3 {tools / 'pty-escape-probe.py'} {marker}")
    assert status == 0, output
    report = json.loads(next(line for line in output.splitlines() if marker in line and "{" in line))
    expected = f"/{daemon.root.relative_to('/sys/fs/cgroup')}/{group_of(spawned)}"
    assert report["start"] == expected and report["cgroup"] == expected, report
    results = report["results"]
    assert "error" not in results, results  # every attempt must actually run
    escaped = {name: outcome for name, outcome in results.items()
               if not outcome.startswith(("denied:", "contained", "unavailable:"))}
    assert not escaped, escaped
    for attempt in ("userns", "cgroupns"):
        assert any(name.startswith(attempt) for name in results), (attempt, results)
    for pid in members(daemon, spawned):
        assert Path(f"/proc/{pid}/cgroup").read_text().strip().endswith(group_of(spawned))
    # Processes inside a new user namespace are still Session members.
    shell(daemon, spawned, "unshare -U -r sh -c 'echo NESTED:$$; exec sleep 300' &")
    found = None
    for _ in range(100):
        data = recovery.base64.b64decode(ok(daemon.socket, "session.read", {
            "id": spawned["id"], "cursor": 0,
            "control_token": spawned["control_token"]})["output_base64"])
        found = re.search(rb"NESTED:(\d+)\r?\n", data)
        if found:
            break
        time.sleep(0.05)
    held = {}
    if found:
        pid = int(found.group(1))
        held[pid] = identity(pid)
    processes = {pid: identity(pid) for pid in members(daemon, spawned)} | held
    assert terminate(daemon, spawned)["type"] == "ok"
    wait_gone(processes, "terminate with user-namespace descendants")
    assert_session_gone(daemon, spawned, "namespace escape")
    userns = "available" if found else "unavailable"
    print(f"PTY #72 namespace escape: parent, management and host cgroup.procs denied "
          f"directly, from a user namespace and from a user+mount+cgroup namespace "
          f"(view contained, mkdir denied); setns into PID 1 namespaces denied; "
          f"unprivileged user namespaces {userns}; terminate killed every member: "
          + json.dumps(report["results"], sort_keys=True))


def reuse_pid(helper, pid):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(20)
        client.connect(helper)
        client.sendall(json.dumps({"pid": pid}).encode())
        reply = json.loads(client.recv(256))
    assert reply.get("pid") == pid, reply
    return pid


def case_double_close_and_pid_reuse(daemon, helper):
    spawned = spawn(daemon)
    first = request(daemon.socket, "session.detach", {
        "id": spawned["id"], "attachment_token": spawned["writer"]})
    second = request(daemon.socket, "session.detach", {
        "id": spawned["id"], "attachment_token": spawned["writer"]})
    assert first["type"] == "ok" and second["type"] == "err", (first, second)
    assert second.get("code") != "internal", second
    with ThreadPoolExecutor(max_workers=2) as pool:
        replies = list(pool.map(lambda _: terminate(daemon, spawned), range(2)))
    assert [reply["type"] for reply in replies].count("ok") >= 1, replies
    assert all(reply["type"] == "ok" or reply.get("code") not in (None, "internal")
               for reply in replies), replies
    assert_session_gone(daemon, spawned, "double terminate")
    third = terminate(daemon, spawned)
    assert third["type"] == "err" and third.get("code") != "internal", third
    responsive(daemon)

    victim = spawn(daemon)
    shell_pid, guardian_pid = victim["pid"], victim["supervisor_pid"]
    original = identity(shell_pid)
    request(daemon.socket, "session.write", {"id": victim["id"],
                                             "attachment_token": victim["writer"],
                                             "data": "exit\n"})
    deadline = time.monotonic() + 10
    while alive(shell_pid, original) and time.monotonic() < deadline:
        time.sleep(0.02)
    assert not alive(shell_pid, original), "shell did not exit"
    while identity(shell_pid) and time.monotonic() < deadline:
        time.sleep(0.02)  # wait until the zombie is reaped and the PID is free
    decoy = reuse_pid(helper, shell_pid)
    decoy_identity = identity(decoy)
    assert decoy_identity[1] != original[1], "decoy must be a new process"
    for name, args in (("session.terminate", {"id": victim["id"],
                                              "control_token": victim["control_token"]}),
                       ("session.heartbeat", {"id": victim["id"],
                                              "attachment_token": victim["writer"]})):
        request(daemon.socket, name, args)
    time.sleep(2.5)  # two reconcile intervals
    assert alive(decoy, decoy_identity), "stale terminate or reconcile signalled a reused PID"
    daemon.kill()
    daemon.start()
    time.sleep(1.5)
    assert alive(decoy, decoy_identity), "restart recovery signalled a reused PID"
    print(f"PTY #72 double-close and PID reuse: second detach and terminate typed, never "
          f"internal; a decoy born with the exited shell's PID {shell_pid} survived stale "
          f"terminate/heartbeat, reconcile, flowerlyd SIGKILL and restart recovery "
          f"(guardian was {guardian_pid})")


def memory_events(daemon, spawned):
    text = (daemon.root / group_of(spawned) / "memory.events").read_text()
    return dict(line.split() for line in text.splitlines())


def case_resource_exhaustion(daemon):
    hostile, bystander = spawn(daemon), spawn(daemon)
    group = daemon.root / group_of(hostile)
    output, _ = shell(daemon, hostile, (
        "python3 -c \"import os,time\n"
        "n=0\n"
        "try:\n"
        "    while True:\n"
        "        if os.fork()==0:\n"
        "            time.sleep(60); os._exit(0)\n"
        "        n+=1\n"
        "except OSError as e:\n"
        "    print('FORKS', n, e.errno, flush=True)\n"
        "\" &"))
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        events = dict(line.split() for line in (group / "pids.events").read_text().splitlines())
        if int(events.get("max", 0)) > 0:
            break
        time.sleep(0.05)
    assert int(events.get("max", 0)) > 0, events
    forks_refused = events["max"]
    assert int((group / "pids.current").read_text()) <= 64
    latency = responsive(daemon)
    echo, status = shell(daemon, bystander, "echo BYSTANDER_OK")
    assert status == 0 and "BYSTANDER_OK" in echo
    request(daemon.socket, "session.write", {"id": hostile["id"],
                                             "attachment_token": hostile["writer"],
                                             "data": "\x03pkill -f 'import os,time' ; true\n"})
    terminate(daemon, hostile)
    assert_session_gone(daemon, hostile, "fork bomb")

    hog = spawn(daemon)
    shell(daemon, hog, "python3 -c \"b=[]\nwhile True: b.append(bytearray(64<<20))\"; true",
          seconds=60)
    events = memory_events(daemon, hog)
    assert int(events.get("oom_kill", 0)) >= 1, events
    responsive(daemon)
    spinner = spawn(daemon)
    shell(daemon, spinner, "for i in 1 2 3 4; do (while :; do :; done) & done")
    time.sleep(3)
    cpu = dict(line.split() for line in
               (daemon.root / group_of(spinner) / "cpu.stat").read_text().splitlines())
    assert int(cpu.get("nr_throttled", 0)) > 0, cpu
    responsive(daemon)
    flood = spawn(daemon)
    request(daemon.socket, "session.write", {"id": flood["id"],
                                             "attachment_token": flood["writer"],
                                             "data": "timeout 3 yes FLOOD\n"})
    time.sleep(3.5)
    replay = recovery.base64.b64decode(ok(daemon.socket, "session.read", {
        "id": flood["id"], "cursor": 0, "control_token": flood["control_token"]})["output_base64"])
    assert len(replay) <= 1024 * 1024, len(replay)
    rss = int(re.search(r"VmRSS:\s+(\d+)", Path(f"/proc/{daemon.process.pid}/status")
                        .read_text()).group(1))
    assert rss < 200 * 1024, f"flowerlyd RSS {rss} kB after output flood"
    responsive(daemon)
    for spawned in (hog, spinner, flood, bystander):
        terminate(daemon, spawned)
        assert_session_gone(daemon, spawned, "resource exhaustion")
    print(f"PTY #72 hostile resource exhaustion: fork bomb stopped at pids.max "
          f"(pids.events max={forks_refused}), memory hog OOM-killed in its cgroup "
          f"(oom_kill={events['oom_kill']}), 4 spinners throttled "
          f"(nr_throttled={cpu['nr_throttled']}), output flood bounded "
          f"(replay {len(replay)} B, flowerlyd RSS {rss} kB); flowerlyd answered in "
          f"{latency:.3f}s and a bystander Session kept working")


def case_zombies_and_orphans(daemon):
    spawned = spawn(daemon)
    output, _ = shell(daemon, spawned, (
        "(sleep 300 &) ; setsid sleep 300 & "
        "python3 -c \"import os,time\nif os.fork()==0: os._exit(0)\ntime.sleep(300)\" & "
        "sleep 0.5; ps -o stat= -u $(id -u) | grep -c Z"))
    zombies = int(re.findall(r"^(\d+)\s*$", output, re.M)[-1])
    assert zombies >= 1, output
    held = {pid: identity(pid) for pid in members(daemon, spawned)}
    assert len(held) >= 4, held
    assert terminate(daemon, spawned)["type"] == "ok"
    wait_gone(held, "orphans and zombies after terminate")
    assert_session_gone(daemon, spawned, "orphans and zombies")

    natural = spawn(daemon)
    shell(daemon, natural, "setsid sleep 300 & (sleep 300 &)")
    held = {pid: identity(pid) for pid in members(daemon, natural)}
    request(daemon.socket, "session.write", {"id": natural["id"],
                                             "attachment_token": natural["writer"],
                                             "data": "exit\n"})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        row = daemon.sessions()[natural["id"]]
        if row["state"].lower() == "exited":
            break
        time.sleep(0.1)
    assert row["state"].lower() == "exited", row
    exited_at = time.monotonic()
    survivors = {pid for pid, original in held.items() if alive(pid, original)}
    # No terminate: the guardian keeps a 20 s replay window after the shell
    # exits, then flowerlyd ends it and its cgroup.kill takes the orphans.
    wait_gone(held, "background jobs of a naturally exited shell", seconds=30)
    reaped_after = time.monotonic() - exited_at
    assert_session_gone(daemon, natural, "natural exit")
    time.sleep(1.5)
    zombie_guardians = [pid for pid in children(daemon.process.pid)
                        if (identity(pid) or ("X",))[0] == "Z"]
    assert not zombie_guardians, zombie_guardians
    print(f"PTY #72 zombies and orphans: double-forked and setsid orphans and an unreaped "
          f"zombie ({zombies} Z) killed by terminate; after a natural shell exit "
          f"{len(survivors)} contained background job(s) were killed without any client "
          f"action {reaped_after:.1f}s later (20 s replay window); no zombie guardian remains")


def case_fail_closed(daemon):
    control = daemon.root / "cgroup.subtree_control"
    before = uid_processes(SHELL_UID)
    groups = daemon.pty_groups()
    control.write_text("-pids")
    try:
        refused = request(daemon.socket, "session.spawn", {"shell": "/bin/sh"})
        assert refused["type"] == "err", refused
        assert refused.get("code") != "internal" or "PTY" in json.dumps(refused), refused
        time.sleep(0.5)
        assert daemon.pty_groups() == groups, daemon.pty_groups()
        started = set(uid_processes(SHELL_UID)) - set(before)
        assert not started, f"a shell started without pids containment: {started}"
    finally:
        control.write_text("+pids")
    accepted = spawn(daemon)
    terminate(daemon, accepted)
    assert_session_gone(daemon, accepted, "fail closed")
    print("PTY #72 fail closed: without the pids controller a spawn is refused "
          f"({refused.get('code')}) before any shell-UID process starts and leaves no "
          "cgroup; spawning resumes once containment can be established")


def main(binary_directory):
    assert sys.platform == "linux" and os.geteuid() != 0 and SHELL_UID > 0
    helper = os.environ.get("FLOWERLY_PTY_PID_REUSE_HELPER")
    assert helper and stat.S_ISSOCK(os.stat(helper).st_mode), "PID-reuse helper socket required"
    tools = Path(tempfile.mkdtemp(prefix="flowerly-pty-probe-"))
    os.chmod(tools, 0o755)
    probe = tools / "pty-escape-probe.py"
    probe.write_text(Path(__file__).with_name("pty-escape-probe.py").read_text())
    probe.chmod(0o644)
    daemon = recovery.Daemon(binary_directory, Path(os.environ["FLOWERLY_PTY_CGROUP_ROOT"]))
    try:
        daemon.start()
        case_namespace_escape(daemon, tools)
        case_double_close_and_pid_reuse(daemon, helper)
        case_resource_exhaustion(daemon)
        case_zombies_and_orphans(daemon)
        case_fail_closed(daemon)
    except BaseException:
        sys.stderr.write(daemon.log.read_text()[-6000:])
        raise
    finally:
        daemon.close()
        probe.unlink()
        tools.rmdir()
    print("PTY_CONTAINMENT_ADVERSARIAL=PASS")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-pty-containment-adversarial.py BINARY_DIR")
    main(Path(sys.argv[1]).resolve())

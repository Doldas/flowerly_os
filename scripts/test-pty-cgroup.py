#!/usr/bin/env python3
import base64
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid

GUARDIAN = Path(__file__).with_name("pty-guardian.py")


def verify_write_only_cgroup_kill_probe():
    """The real cgroup.kill file is write-only (0200), not readable.

    Test the production SessionCgroup opener with a fake directory. We must
    not write "1" to a live cgroup during this capability probe.
    """
    spec = importlib.util.spec_from_file_location("flowerly_pty_guardian", GUARDIAN)
    guardian_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guardian_module)

    with tempfile.TemporaryDirectory(prefix="flowerly-pty-kill-probe-") as folder:
        root = Path(folder)
        kill_file = root / "cgroup.kill"
        kill_file.touch(mode=0o600)
        kill_file.chmod(0o200)
        fd = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            probe = guardian_module.SessionCgroup.__new__(guardian_module.SessionCgroup)
            probe.group_fd = fd

            # An unprivileged service account must be able to probe a
            # WRITE-ONLY cgroup.kill without reading or modifying its contents.
            probe.require_writable("cgroup.kill")
            assert kill_file.stat().st_size == 0, "capability probe wrote to cgroup.kill"

            # A read-only kill file must never be accepted: cleanup would fail.
            kill_file.chmod(0o400)
            if os.geteuid() != 0:
                try:
                    probe.require_writable("cgroup.kill")
                except RuntimeError as exc:
                    assert "not writable" in str(exc), str(exc)
                else:
                    raise AssertionError("read-only cgroup.kill passed writable probe")

            kill_file.unlink()
            try:
                probe.require_writable("cgroup.kill")
            except RuntimeError:
                pass
            else:
                raise AssertionError("missing cgroup.kill passed writable probe")

            # No symlink traversal even if the target is writable.
            target = root / "not-cgroup-kill"
            target.touch(mode=0o600)
            kill_file.symlink_to(target.name)
            try:
                probe.require_writable("cgroup.kill")
            except RuntimeError:
                pass
            else:
                raise AssertionError("symlink cgroup.kill passed writable probe")
        finally:
            os.close(fd)

    print("cgroup.kill write-only capability, fail-closed and no-symlink probes OK")


def verify_writable_migration_targets_are_rejected():
    """Check access refusal only; real kernel confinement needs Arch tests."""
    spec = importlib.util.spec_from_file_location("flowerly_pty_guardian", GUARDIAN)
    guardian_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guardian_module)
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-escape-probe-") as folder:
        root = Path(folder)
        management = root / "management"
        management.mkdir()
        (root / "cgroup.procs").write_bytes(b"")
        (management / "cgroup.procs").write_bytes(b"")
        root_fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            probe = guardian_module.SessionCgroup.__new__(guardian_module.SessionCgroup)
            probe.parent_fd = root_fd
            try:
                probe.reject_writable_escape_targets()
            except RuntimeError as error:
                assert "unsafe PTY cgroup delegation" in str(error), str(error)
            else:
                raise AssertionError("writable parent cgroup.procs accepted")
            assert (root / "cgroup.procs").read_bytes() == b""
            assert (management / "cgroup.procs").read_bytes() == b""
        finally:
            os.close(root_fd)
    print("Same-UID writable migration target refused before PTY spawn")


def verify_shell_uid_configuration_rejected():
    """An opt-in split identity must never be root or the guardian UID."""
    spec = importlib.util.spec_from_file_location("flowerly_pty_guardian", GUARDIAN)
    guardian_module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(guardian_module)
    from unittest.mock import patch
    for raw in ("0", str(os.geteuid()), "not-a-uid"):
        with patch.dict(os.environ, {"FLOWERLY_PTY_SHELL_UID": raw}):
            try:
                guardian_module.shell_credentials()
            except (PermissionError, ValueError):
                pass
            else:
                raise AssertionError(f"unsafe PTY shell UID accepted: {raw}")
    print("PTY shell UID configuration: root and same-UID identities refused")


def request(path, command):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(4)
        client.connect(str(path))
        client.sendall(json.dumps(command).encode() + b"\n")
        response = json.loads(client.makefile("rb").readline())
        assert "error" not in response, response
        return response


def assert_not_running(pid):
    for _ in range(150):
        try:
            with open(f"/proc/{pid}/stat", encoding="ascii") as state_file:
                state = state_file.read().split(") ", 1)[1][0]
        except FileNotFoundError:
            return
        if state in ("Z", "X"):
            return
        time.sleep(0.02)
    raise AssertionError(f"process {pid} survived cgroup termination")


def require_explicit_delegation(root):
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-strict-") as folder:
        path = Path(folder) / "strict.sock"
        env = dict(os.environ, FLOWERLY_PTY_ISOLATION="required")
        env.pop("FLOWERLY_PTY_CGROUP_ROOT", None)
        result = subprocess.run(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, capture_output=True, timeout=5,
        )
        assert result.returncode != 0, "guardian silently bypassed required cgroup"
        assert not path.exists(), "guardian exposed control socket without a cgroup"
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-invalid-root-") as folder:
        path = Path(folder) / "invalid.sock"
        env = dict(os.environ, FLOWERLY_PTY_ISOLATION="required",
                   FLOWERLY_PTY_CGROUP_ROOT=folder)
        result = subprocess.run(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, capture_output=True, timeout=5,
        )
        assert result.returncode != 0 and not path.exists(), "ordinary directory bypassed cgroup validation"
        env["FLOWERLY_PTY_CGROUP_NAME"] = "../../unsafe"
        result = subprocess.run(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, capture_output=True, timeout=5,
        )
        assert result.returncode != 0 and not path.exists(), "untrusted cgroup name accepted"
        assert b"invalid supervised cgroup identity" in result.stderr, result.stderr
    if root is None:
        print("Production fail-closed verified; delegated cgroup e2e NOT VERIFIED")


def assert_shell_cannot_migrate_out_of_cgroup(path, root, group_name):
    destinations = (
        ("MANAGEMENT", Path(root) / "management" / "cgroup.procs"),
        ("PARENT", Path(root) / "cgroup.procs"),
    )
    for label, destination in destinations:
        assert destination.is_file(), f"missing migration target: {destination}"
        source = (
            "import os\n"
            "from pathlib import Path\n"
            f"group_name = {group_name!r}\n"
            f"destination = {str(destination)!r}\n"
            f"label = {label!r}\n"
            "membership = Path('/proc/self/cgroup').read_text().strip()\n"
            "if not membership.endswith('/' + group_name):\n"
            "    print('PTY_MIGRATION_' + label + '_INVALID:0', flush=True)\n"
            "else:\n"
            "    try:\n"
            "        with open(destination, 'w', encoding='ascii') as target:\n"
            "            target.write(str(os.getpid()))\n"
            "    except OSError as error:\n"
            "        print('PTY_MIGRATION_' + label + '_DENIED:' + str(error.errno), flush=True)\n"
            "    else:\n"
            "        print('PTY_MIGRATION_' + label + '_ESCAPED:0', flush=True)\n"
        )
        encoded = base64.b64encode(source.encode("utf-8")).decode("ascii")
        command = (
            "python3 -c 'import base64; exec(base64.b64decode(\"" + encoded + "\"))'\n"
        ).encode()
        assert request(path, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
        pattern = re.compile(
            rb"PTY_MIGRATION_" + label.encode("ascii") + rb"_(DENIED|ESCAPED|INVALID):(\d+)"
        )
        for _ in range(150):
            reply = request(path, {"op": "read", "cursor": 0})
            match = pattern.search(base64.b64decode(reply["output_base64"]))
            if match:
                outcome = match.group(1)
                error = int(match.group(2))
                permitted_errors = (1, 13) if label == "MANAGEMENT" else (1, 13, 95)
                assert outcome == b"DENIED" and error in permitted_errors, (
                    f"{label} migration bypassed session containment or returned an unexpected result: "
                    f"{outcome!r}, errno={error}"
                )
                print(f"Delegated cgroup: same-UID migration to {label.lower()} denied (errno={error})")
                break
            time.sleep(0.02)
        else:
            raise AssertionError(f"{label} cgroup migration probe did not return a result")


def exercise_real_cgroup(root):
    with tempfile.TemporaryDirectory(prefix="flowerly-pty-cgroup-") as folder:
        path = Path(folder) / "cgroup.sock"
        group_name = "pty-session-" + uuid.uuid4().hex
        group = Path(root) / group_name
        env = dict(os.environ, FLOWERLY_PTY_ISOLATION="required",
                   FLOWERLY_PTY_CGROUP_ROOT=root,
                   FLOWERLY_PTY_CGROUP_NAME=group_name)
        guardian = subprocess.Popen(
            [sys.executable, str(GUARDIAN), str(path), "/bin/sh", str(os.getpid())],
            env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        descendant_pid = None
        try:
            for _ in range(150):
                if guardian.poll() is not None:
                    raise AssertionError(guardian.stderr.read().decode())
                if path.exists():
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("cgroup guardian startup timeout")
            identity = request(path, {"op": "status"})
            shell_pid = identity["pid"]
            assert group.is_dir(), group
            assert (group / "pids.max").read_text().strip() == "64"
            assert (group / "memory.max").read_text().strip() == str(512 * 1024 * 1024)
            assert (group / "cpu.max").read_text().strip() == "100000 100000"
            assert str(shell_pid) in (group / "cgroup.procs").read_text().split()
            expected_shell_uid = os.environ.get("FLOWERLY_PTY_SHELL_UID")
            if expected_shell_uid is not None:
                expected_shell_uid = int(expected_shell_uid)
                # Inspect kernel credentials, not client-controlled PTY output.
                for _ in range(150):
                    status = Path(f"/proc/{shell_pid}/status").read_text()
                    fields = dict(
                        line.split(":", 1) for line in status.splitlines() if ":" in line
                    )
                    uids = tuple(map(int, fields["Uid"].split()))
                    if uids == (expected_shell_uid,) * 4:
                        break
                    time.sleep(0.02)
                else:
                    raise AssertionError("PTY shell did not assume separate UID")
                for key in ("CapEff", "CapPrm", "CapAmb"):
                    assert int(fields[key].strip(), 16) == 0, (
                        f"PTY shell retains privileged capabilities: {key}"
                    )
                assert fields["Groups"].strip() == "", (
                    "PTY shell retained supplementary groups"
                )
                print("PTY shell: separate UID, no effective/permitted/ambient capabilities")
            assert_shell_cannot_migrate_out_of_cgroup(path, root, group_name)
            command = (
                b"python3 -c 'import subprocess; p=subprocess.Popen("
                b"[\"sleep\",\"60\"],start_new_session=True);"
                b"print(\"FLOWERLY_SETSID:%s\"%p.pid,flush=True)'\n"
            )
            assert request(path, {"op": "write", "data_hex": command.hex()})["written"] == len(command)
            for _ in range(150):
                reply = request(path, {"op": "read", "cursor": 0})
                match = re.search(
                    rb"FLOWERLY_SETSID:(\d+)",
                    base64.b64decode(reply["output_base64"]),
                )
                if match:
                    descendant_pid = int(match.group(1))
                    break
                time.sleep(0.02)
            assert descendant_pid, "escaped-session process PID not reported"
            assert os.getsid(descendant_pid) != shell_pid, "child did not escape terminal session"
            guardian.kill()
            guardian.wait(timeout=5)
            assert_not_running(shell_pid)
            assert_not_running(descendant_pid)
            for _ in range(100):
                if not group.exists():
                    break
                time.sleep(.02)
            else:
                raise AssertionError("owned PTY cgroup was not removed after guardian death")
            print("Real delegated cgroup v2: setsid descendant terminated after guardian SIGKILL")
        finally:
            if guardian.poll() is None:
                guardian.kill()
                guardian.wait(timeout=5)
            if descendant_pid is not None:
                try:
                    os.kill(descendant_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


def main():
    assert sys.platform == "linux"
    root = os.environ.get("FLOWERLY_PTY_CGROUP_ROOT")
    verify_write_only_cgroup_kill_probe()
    verify_writable_migration_targets_are_rejected()
    verify_shell_uid_configuration_rejected()
    require_explicit_delegation(root)
    if root:
        exercise_real_cgroup(root)


if __name__ == "__main__":
    main()

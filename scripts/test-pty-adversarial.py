#!/usr/bin/env python3
import base64
import errno
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import re
import signal
import stat
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import uuid


def receive_exact(connection, size):
    data = bytearray()
    while len(data) < size:
        fragment = connection.recv(size - len(data))
        if not fragment:
            raise AssertionError("sessiond IPC response truncated")
        data.extend(fragment)
    return bytes(data)


def request(path, name, arguments, timeout=12):
    identifier = str(uuid.uuid4())
    payload = json.dumps({
        "version": 1,
        "id": identifier,
        "type": "command",
        "name": name,
        "args": arguments,
    }).encode()
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(str(path))
        connection.sendall(struct.pack("<I", len(payload)) + payload)
        length = struct.unpack("<I", receive_exact(connection, 4))[0]
        assert length <= 1024 * 1024, length
        result = json.loads(receive_exact(connection, length))
    assert result["id"] == identifier and result["version"] == 1, result
    return result


def successful(path, name, arguments):
    result = request(path, name, arguments)
    assert result["type"] == "ok", (name, result)
    return result["data"]


def process_identity(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    except FileNotFoundError:
        return None
    fields = stat.rsplit(") ", 1)[1].split()
    return fields[0], fields[19]


def assert_original_process_exited(pid, identity):
    for _ in range(200):
        current = process_identity(pid)
        if current is None or current[1] != identity[1] or current[0] in ("Z", "X"):
            return
        time.sleep(0.025)
    raise AssertionError(f"original supervised process {pid} survived daemon SIGKILL")


class SessionDaemon:
    def __init__(self, binary_directory):
        self.binary = binary_directory / "sessiond"
        self.temporary = tempfile.TemporaryDirectory(prefix="flowerly-pty-adversarial-")
        self.root = Path(self.temporary.name)
        self.runtime = self.root / "runtime"
        self.state = self.root / "state"
        self.runtime.mkdir(mode=0o700)
        self.state.mkdir(mode=0o700)
        self.socket_path = self.runtime / "sessiond.sock"
        self.environment = dict(
            os.environ,
            AGENTIC_RUNTIME_DIR=str(self.runtime),
            AGENTIC_STATE_DIR=str(self.state),
            FLOWERLY_PTY_ISOLATION="development",
        )
        self.process = None

    def start(self):
        with (self.root / "sessiond.log").open("ab") as log:
            self.process = subprocess.Popen(
                [str(self.binary)],
                env=self.environment,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        for _ in range(200):
            if self.process.poll() is not None:
                raise AssertionError(
                    "sessiond died on startup: "
                    + (self.root / "sessiond.log").read_text()
                )
            if self.socket_path.exists():
                try:
                    successful(self.socket_path, "session.list", {})
                    return
                except (OSError, AssertionError):
                    pass
            time.sleep(0.03)
        raise AssertionError(
            "sessiond readiness timeout: " + (self.root / "sessiond.log").read_text()
        )

    def stop(self, abrupt=False):
        if self.process is None or self.process.poll() is not None:
            return
        if abrupt:
            self.process.kill()
        else:
            self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)

    def close(self):
        self.stop()
        self.temporary.cleanup()

    def recover_socket_after_abrupt_stop(self):
        assert self.process is not None and self.process.poll() is not None
        runtime = self.runtime.stat()
        assert runtime.st_uid == os.geteuid() and stat.S_IMODE(runtime.st_mode) == 0o700
        original = self.socket_path.lstat()
        assert stat.S_ISSOCK(original.st_mode) and original.st_uid == os.geteuid()
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(1)
            try:
                connection.connect(str(self.socket_path))
            except OSError as error:
                assert error.errno == errno.ECONNREFUSED, error
            else:
                raise AssertionError("refusing recovery of live IPC socket")
        current = self.socket_path.lstat()
        assert (current.st_dev, current.st_ino) == (
            original.st_dev, original.st_ino
        ), "stale socket pathname was replaced before recovery"
        self.socket_path.unlink()

    def spawn(self):
        response = successful(self.socket_path, "session.spawn", {"shell": "/bin/sh"})
        assert response["pid"] > 0 and response["supervisor_pid"] > 0, response
        assert response["control_token"], response
        return response


def output(response):
    return base64.b64decode(response["output_base64"])


def write(session, identifier, token, data):
    return successful(session, "session.write", {
        "id": identifier, "attachment_token": token, "data": data,
    })


def test_independent_readers_backpressure_and_disconnection(daemon):
    session = daemon.socket_path
    spawned = daemon.spawn()
    identifier = spawned["id"]
    control = spawned["control_token"]
    attachment = successful(session, "session.attach", {
        "id": identifier, "control_token": control,
    })
    token = attachment["attachment_token"]
    try:
        burst = 'python3 -c ' + "'import sys; sys.stdout.write(\"Q\"*180000); sys.stdout.flush()'" + "\n"
        assert write(session, identifier, token, burst)["written"] == len(burst)
        for _ in range(250):
            status = successful(session, "session.status", {"id": identifier})
            if status["cursor"] > 160000:
                break
            time.sleep(0.025)
        else:
            raise AssertionError("burst output did not reach bounded replay")

        def historical_reader(_):
            return successful(session, "session.read", {
                "id": identifier, "cursor": 0, "control_token": control,
            })

        with ThreadPoolExecutor(max_workers=8) as pool:
            historical = list(pool.map(historical_reader, range(16)))
        assert all(reply["truncated"] for reply in historical), historical
        assert all(0 < len(output(reply)) <= 65536 for reply in historical)
        assert len({reply["cursor"] for reply in historical}) == 1, historical

        cursor = historical[-1]["cursor"]
        marker = "FLOWERLY_MULTIREADER_FOLLOW"
        assert write(session, identifier, token, f"echo {marker}\n")["written"] > 0

        def independent_follower(_):
            position = cursor
            for _ in range(12):
                reply = successful(session, "session.follow", {
                    "id": identifier, "control_token": control,
                    "cursor": position, "wait_ms": 200,
                })
                if marker.encode() in output(reply):
                    return reply
                position = reply["cursor"]
            raise AssertionError("independent reader did not observe new PTY output")

        with ThreadPoolExecutor(max_workers=6) as pool:
            followers = list(pool.map(independent_follower, range(6)))
        assert all(marker.encode() in output(reply) for reply in followers)

        follower_cursor = successful(session, "session.status", {
            "id": identifier,
        })["cursor"]
        disconnect_payload = json.dumps({
            "version": 1,
            "id": str(uuid.uuid4()),
            "type": "command",
            "name": "session.follow",
            "args": {
                "id": identifier,
                "control_token": control,
                "cursor": follower_cursor,
                "wait_ms": 250,
            },
        }).encode()

        def abandon_response(_):
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(5)
                connection.connect(str(session))
                connection.sendall(
                    struct.pack("<I", len(disconnect_payload)) + disconnect_payload
                )

        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(abandon_response, range(8)))

        assert successful(session, "session.status", {"id": identifier})["running"]
        assert successful(session, "session.heartbeat", {
            "id": identifier, "attachment_token": token,
        })["lease_active"]

        successful(session, "session.detach", {
            "id": identifier, "attachment_token": token,
        })
        stale = request(session, "session.write", {
            "id": identifier, "attachment_token": token, "data": "echo stale\n",
        })
        assert stale["type"] == "err", stale
        replacement = successful(session, "session.attach", {
            "id": identifier, "control_token": control,
        })
        new_token = replacement["attachment_token"]
        assert new_token and new_token != token
        operations = [
            ("session.follow", {"id": identifier, "control_token": control,
                                "cursor": follower_cursor, "wait_ms": 0}),
            ("session.resize", {"id": identifier, "attachment_token": new_token,
                                "rows": 38, "cols": 100}),
            ("session.heartbeat", {"id": identifier, "attachment_token": new_token}),
            ("session.terminate", {"id": identifier, "control_token": control}),
        ]
        with ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda entry: request(session, *entry), operations))
        assert any(result["type"] == "ok" for result in results[:3]), results
        assert results[-1]["type"] == "ok" and results[-1]["data"]["terminated"], results
        assert successful(session, "session.list", {})["count"] >= 1
        print("PTY multi-client: bounded replay, independent followers, disconnect, writer revocation, concurrent terminate")
    finally:
        try:
            successful(session, "session.terminate", {
                "id": identifier, "control_token": control,
            })
        except AssertionError:
            pass


def test_failed_durable_spawn_reaps_guardian(daemon):
    session = daemon.socket_path
    database = daemon.state / "state.db"
    known_sessions = {
        entry["id"] for entry in successful(session, "session.list", {})["sessions"]
    }
    known_sockets = set(daemon.runtime.glob("pty-*.sock"))
    with sqlite3.connect(database, isolation_level=None) as connection:
        connection.execute(
            "CREATE TRIGGER reject_pty_session_insert BEFORE INSERT ON sessions "
            "BEGIN SELECT RAISE(ABORT, 'deliberate durable insertion failure'); END"
        )
    try:
        rejected = request(session, "session.spawn", {"shell": "/bin/sh"})
        assert rejected["type"] == "err", rejected
    finally:
        with sqlite3.connect(database, isolation_level=None) as connection:
            connection.execute("DROP TRIGGER reject_pty_session_insert")

    for _ in range(100):
        new_sockets = set(daemon.runtime.glob("pty-*.sock")) - known_sockets
        reachable = []
        for path in new_sockets:
            with socket.socket(socket.AF_UNIX) as probe:
                probe.settimeout(0.25)
                try:
                    probe.connect(str(path))
                except OSError:
                    continue
                reachable.append(path)
        if not reachable:
            break
        time.sleep(0.03)
    else:
        raise AssertionError(
            f"unregistered PTY guardians remained reachable after SQLite failure: {reachable}"
        )
    after = {
        entry["id"] for entry in successful(session, "session.list", {})["sessions"]
    }
    assert after == known_sessions, (known_sessions, after)
    replacement = daemon.spawn()
    successful(session, "session.terminate", {
        "id": replacement["id"],
        "control_token": replacement["control_token"],
    })
    print("PTY failed SQLite spawn: uncommitted guardian stopped, no durable ghost, subsequent spawn succeeds")


def test_abrupt_daemon_loss_and_restart(daemon):
    session = daemon.socket_path
    spawned = daemon.spawn()
    identifier = spawned["id"]
    control = spawned["control_token"]
    shell_pid = spawned["pid"]
    supervisor_pid = spawned["supervisor_pid"]
    shell_identity = process_identity(shell_pid)
    supervisor_identity = process_identity(supervisor_pid)
    assert shell_identity is not None and supervisor_identity is not None
    writer = successful(session, "session.attach", {
        "id": identifier, "control_token": control,
    })["attachment_token"]
    background_command = "sleep 60 & echo FLOWERLY_ABRUPT_JOB:$!\n"
    write(session, identifier, writer, background_command)
    job_pid = None
    for _ in range(120):
        reply = successful(session, "session.read", {
            "id": identifier, "cursor": 0, "control_token": control,
        })
        match = re.search(rb"FLOWERLY_ABRUPT_JOB:(\d+)", output(reply))
        if match:
            job_pid = int(match.group(1))
            break
        time.sleep(0.025)
    assert job_pid, "background process did not start"
    job_identity = process_identity(job_pid)
    assert job_identity and job_identity[0] not in ("Z", "X")
    daemon.stop(abrupt=True)
    assert_original_process_exited(shell_pid, shell_identity)
    assert_original_process_exited(job_pid, job_identity)
    assert_original_process_exited(supervisor_pid, supervisor_identity)

    try:
        daemon.start()
    except AssertionError as error:
        assert "Address already in use" in str(error), error
        daemon.recover_socket_after_abrupt_stop()
        daemon.start()
        print("PTY daemon SIGKILL: stale IPC socket refused until guarded operator recovery")

    historical = next(row for row in successful(
        session, "session.list", {}
    )["sessions"] if row["id"] == identifier)
    assert historical["state"].lower() == "suspended", historical
    assert historical["presentation_state"].lower() == "detached", historical
    for name, args in (
        ("session.attach", {"id": identifier, "control_token": control}),
        ("session.read", {"id": identifier, "control_token": control, "cursor": 0}),
        ("session.terminate", {"id": identifier, "control_token": control}),
        ("session.heartbeat", {"id": identifier, "attachment_token": writer}),
    ):
        outcome = request(session, name, args)
        assert outcome["type"] == "err", (name, outcome)
    successor = daemon.spawn()
    assert successor["id"] != identifier
    successful(session, "session.terminate", {
        "id": successor["id"], "control_token": successor["control_token"],
    })
    print("PTY abrupt SIGKILL: original shell/job/guardian exited, stale Session unavailable, fresh spawn succeeds")


def main(binary_directory):
    assert sys.platform == "linux" and os.geteuid() != 0
    daemon = SessionDaemon(binary_directory)
    try:
        daemon.start()
        test_independent_readers_backpressure_and_disconnection(daemon)
        test_failed_durable_spawn_reaps_guardian(daemon)
        test_abrupt_daemon_loss_and_restart(daemon)
    finally:
        daemon.close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: test-pty-adversarial.py target/release")
    main(Path(sys.argv[1]).resolve())

#!/usr/bin/env python3
"""Experimental Linux PTY ownership test. NOT a Flowerly Session backend.

Creates a real session leader with forkpty, keeps the master open while the
simulated frontend detaches, observes output after reattachment, then terminates
and reaps the owned process group. Run only on Linux as an unprivileged user.
"""
import os
import select
import signal
import sys
import time


def read_until(fd: int, needle: bytes, timeout: float = 4.0) -> bytes:
    observed = bytearray()
    deadline = time.monotonic() + timeout
    while needle not in observed:
        left = deadline - time.monotonic()
        if left <= 0:
            raise AssertionError(f"timed out waiting for {needle!r}; got {observed!r}")
        readable, _, _ = select.select([fd], [], [], left)
        if not readable:
            continue
        try:
            data = os.read(fd, 4096)
        except OSError as exc:
            raise AssertionError(f"PTY closed before {needle!r}") from exc
        if not data:
            raise AssertionError("PTY reached EOF unexpectedly")
        observed.extend(data)
        if len(observed) > 65536:
            raise AssertionError("experimental output cap exceeded")
    return bytes(observed)


def main() -> None:
    if sys.platform != "linux":
        raise SystemExit("Linux only")
    pid, master = os.forkpty()
    if pid == 0:
        code = (
            "import os,signal,time,sys;"
            "print('CHILD_READY', flush=True);"
            "time.sleep(.15);"
            "print('OUTPUT_DURING_DETACH', flush=True);"
            "time.sleep(30)"
        )
        os.execv(sys.executable, [sys.executable, "-u", "-c", code])

    waited = False
    try:
        read_until(master, b"CHILD_READY")
        # The process group is the new session (forkpty) led by the child.
        if os.getpgid(pid) != pid:
            raise AssertionError("child did not become process-group leader")
        # 'Detach' deliberately drops the frontend consumer, NOT the PTY master.
        time.sleep(0.30)
        if os.waitpid(pid, os.WNOHANG) != (0, 0):
            waited = True
            raise AssertionError("child exited while frontend was detached")
        # 'Reattach': the same owned master and PID deliver pending output.
        output = read_until(master, b"OUTPUT_DURING_DETACH")
        assert b"OUTPUT_DURING_DETACH" in output
        os.killpg(pid, signal.SIGTERM)
        _, status = os.waitpid(pid, 0)
        waited = True
        assert os.WIFSIGNALED(status) and os.WTERMSIG(status) == signal.SIGTERM
        print("PTY spike OK: forkpty, process group, detach, output replay, termination/reap")
    finally:
        if not waited:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
        os.close(master)


if __name__ == "__main__":
    main()

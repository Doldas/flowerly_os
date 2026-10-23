#!/usr/bin/env python3
"""Bounded host/QEMU resource timeline for Arch VM acceptance (#184).

Records numeric counters only, never environment variables, command lines,
SSH material, guest files, or kernel journal entries.
"""
import argparse
import csv
import os
from pathlib import Path
import time

COLUMNS = (
    "elapsed_seconds", "qemu_alive", "qemu_rss_kib",
    "host_mem_available_kib", "host_swap_free_kib",
    "cgroup_memory_current_bytes", "cgroup_oom", "cgroup_oom_kill",
)


def fields(path, keys):
    try:
        lines = Path(path).read_text(encoding="ascii").splitlines()
    except (OSError, UnicodeError):
        return {}
    result = {}
    for line in lines:
        parts = line.replace(":", " ").split()
        if len(parts) >= 2 and parts[0] in keys:
            try:
                result[parts[0]] = int(parts[1])
            except ValueError:
                pass
    return result


def qemu_pid(pidfile):
    try:
        value = Path(pidfile).read_text(encoding="ascii").strip()
        number = int(value)
        return number if number > 1 and str(number) == value else None
    except (OSError, UnicodeError, ValueError):
        return None


def sample(pidfile, proc="/proc", cgroup="/sys/fs/cgroup"):
    pid = qemu_pid(pidfile)
    status = fields(f"{proc}/{pid}/status", {"VmRSS"}) if pid else {}
    # A missing /proc/<pid>/status means the QEMU process has exited or is
    # not observable; do not mistake this for a live but low-memory guest.
    alive = bool(pid and Path(f"{proc}/{pid}/stat").is_file())
    meminfo = fields(f"{proc}/meminfo", {"MemAvailable", "SwapFree"})
    events = fields(f"{cgroup}/memory.events", {"oom", "oom_kill"})
    current = fields(f"{cgroup}/memory.current", {"current"})
    try:
        current_bytes = int(Path(f"{cgroup}/memory.current").read_text().strip())
    except (OSError, ValueError):
        current_bytes = None
    return {
        "qemu_alive": int(alive),
        "qemu_rss_kib": status.get("VmRSS") if alive else None,
        "host_mem_available_kib": meminfo.get("MemAvailable"),
        "host_swap_free_kib": meminfo.get("SwapFree"),
        "cgroup_memory_current_bytes": current_bytes,
        "cgroup_oom": events.get("oom"),
        "cgroup_oom_kill": events.get("oom_kill"),
    }


def monitor(pidfile, output, interval=10, limit=1000):
    """Append bounded CSV readings; never overwrite existing release evidence."""
    started = time.monotonic()
    with open(output, "x", newline="", encoding="ascii") as stream:
        writer = csv.DictWriter(stream, fieldnames=COLUMNS)
        writer.writeheader()
        for index in range(limit):
            reading = {"elapsed_seconds": int(time.monotonic() - started)}
            reading.update(sample(pidfile))
            writer.writerow(reading)
            stream.flush()
            if index + 1 < limit:
                time.sleep(interval)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pidfile", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--interval", type=int, default=10)
    parser.add_argument("--max-samples", type=int, default=1000)
    args = parser.parse_args()
    if not 1 <= args.interval <= 300 or not 1 <= args.max_samples <= 1000:
        parser.error("interval must be 1..300 and max-samples 1..1000")
    monitor(args.pidfile, args.output, args.interval, args.max_samples)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Classify how an Arch VM acceptance run ended (#184).

Observational only: the classification never changes the run's result.
It reads the numeric facts arch-kernel-vm-test.sh records and the QEMU
serial console, and writes failure-classification.txt. It keeps at most
20 matching serial lines (printable ASCII, 200 characters each), never
environment variables, SSH material, guest files or host journals.

Categories, first match wins:
  success           exit status 0
  host_reboot       the host boot ID changed during the run
  host_oom          QEMU is gone and the runner cgroup's oom_kill grew
  qemu_failure      QEMU is gone without a host OOM kill
  guest_oom         the guest kernel reported an OOM kill
  guest_crash       guest panic/BUG/lockup on the console, or QEMU alive
                    but the guest no longer answers SSH
  ssh_interruption  ssh exited 255 while the guest still answers SSH
  test_failure      a guest command failed (non-255 exit) and the guest
                    still answers SSH
  unknown           none of the above
A host reboot that kills the runner itself cannot be recorded by the run
it interrupts; the next run's host-resource-start.txt shows the boot time.
"""
import argparse
import re
from pathlib import Path

GUEST_OOM = re.compile(r"Out of memory: Killed process|invoked oom-killer|oom-kill:")
GUEST_CRASH = re.compile(r"Kernel panic|BUG: |soft lockup|hard LOCKUP|watchdog: |"
                         r"general protection fault|Oops:|hung_task")
PRINTABLE = re.compile(r"[^\x20-\x7e]")


def serial_matches(path, limit=20):
    try:
        lines = Path(path).read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return [], []
    oom, crash = [], []
    for line in lines:
        clean = PRINTABLE.sub("", re.sub(r"\x1b\[[0-9;]*m", "", line))[:200]
        if GUEST_OOM.search(clean):
            oom.append(clean)
        elif GUEST_CRASH.search(clean):
            crash.append(clean)
    return oom[:limit], crash[:limit]


def classify(facts, oom_lines, crash_lines):
    if facts["exit_status"] == 0:
        return "success"
    if facts["boot_id_start"] and facts["boot_id_end"] \
            and facts["boot_id_start"] != facts["boot_id_end"]:
        return "host_reboot"
    if facts["qemu"] != "alive" and facts["stage"] not in ("before-vm", "shutdown"):
        if facts["oom_kill_end"] > facts["oom_kill_start"]:
            return "host_oom"
        return "qemu_failure"
    if oom_lines:
        return "guest_oom"
    if crash_lines or (facts["qemu"] == "alive" and facts["guest_reachable"] == "no"):
        return "guest_crash"
    if facts["ssh_status"] == 255 and facts["guest_reachable"] == "yes":
        return "ssh_interruption"
    if facts["ssh_status"] not in (None, 0, 255) and facts["guest_reachable"] == "yes":
        return "test_failure"
    return "unknown"


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--report-dir", required=True, type=Path)
    parser.add_argument("--exit-status", required=True, type=int)
    parser.add_argument("--stage", required=True)
    parser.add_argument("--ssh-status", type=int)
    parser.add_argument("--qemu", choices=("alive", "absent", "no_pidfile"), required=True)
    parser.add_argument("--guest-reachable", choices=("yes", "no", "unknown"), default="unknown")
    parser.add_argument("--oom-kill-start", type=int, default=0)
    parser.add_argument("--oom-kill-end", type=int, default=0)
    parser.add_argument("--boot-id-start", default="")
    parser.add_argument("--boot-id-end", default="")
    args = parser.parse_args(argv)
    facts = {key: getattr(args, key) for key in (
        "exit_status", "stage", "ssh_status", "qemu", "guest_reachable",
        "oom_kill_start", "oom_kill_end", "boot_id_start", "boot_id_end")}
    oom_lines, crash_lines = serial_matches(args.report_dir / "qemu-serial.log")
    category = classify(facts, oom_lines, crash_lines)
    lines = [f"category={category}"] + [f"{key}={value}" for key, value in facts.items()]
    lines += [f"guest_oom_line={line}" for line in oom_lines]
    lines += [f"guest_crash_line={line}" for line in crash_lines]
    (args.report_dir / "failure-classification.txt").write_text("\n".join(lines) + "\n")
    print(f"Arch VM run classification: {category}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

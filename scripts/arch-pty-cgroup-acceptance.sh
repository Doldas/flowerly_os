#!/usr/bin/env bash
set -euo pipefail

if [[ $(id -u) -eq 0 ]]; then
  printf '%s\n' "Run delegated PTY acceptance as the intended unprivileged Session user" >&2
  exit 1
fi

if [[ -z "${FLOWERLY_PTY_CGROUP_ROOT:-}" ]]; then
  exec systemd-run --user --scope --collect --property=Delegate=yes \
    "$0" "$@"
fi

if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
  printf '%s\n' "Unified Linux cgroup v2 is required" >&2
  exit 1
fi

export FLOWERLY_PTY_ISOLATION=required
python3 scripts/test-pty-cgroup.py

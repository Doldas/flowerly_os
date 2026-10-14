#!/usr/bin/env bash
set -euo pipefail

if [[ $(id -u) -eq 0 ]]; then
  printf '%s\n' "Run delegated PTY acceptance as the intended unprivileged Session user" >&2
  exit 1
fi

if [[ -z "${FLOWERLY_PTY_CGROUP_ROOT:-}" && "${1:-}" != "--delegated" ]]; then
  exec systemd-run --user --scope --collect --property=Delegate=yes \
    bash "$0" --delegated
fi

if [[ "${1:-}" == "--delegated" ]]; then
  export FLOWERLY_PTY_CGROUP_ROOT="/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)"
fi

if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
  printf '%s\n' "Unified Linux cgroup v2 is required" >&2
  exit 1
fi

if [[ "${1:-}" == "--delegated" ]]; then
  mkdir -p "$FLOWERLY_PTY_CGROUP_ROOT/management"
  printf '%s' "$" > "$FLOWERLY_PTY_CGROUP_ROOT/management/cgroup.procs"
  printf '%s' '+cpu +memory +pids' > "$FLOWERLY_PTY_CGROUP_ROOT/cgroup.subtree_control"
fi

export FLOWERLY_PTY_ISOLATION=required
python3 scripts/test-pty-cgroup.py

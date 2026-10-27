#!/usr/bin/env bash
set -euo pipefail

write_management_pid() {
  local root="$1"
  mkdir -p "$root/management"
  printf '%s' "$BASHPID" > "$root/management/cgroup.procs"
}

if [[ "${1:-}" == "--verify-management-pid" ]]; then
  temporary="$(mktemp -d)"
  trap 'rm -rf "$temporary"' EXIT
  write_management_pid "$temporary"
  [[ "$(cat "$temporary/management/cgroup.procs")" == "$BASHPID" ]] || {
    printf '%s\n' "PTY management cgroup writer did not use the live Bash PID" >&2
    exit 1
  }
  printf '%s\n' "PTY management cgroup PID writer verified"
  exit 0
fi

if [[ "${1:-}" == "--system" ]]; then
  if [[ $(id -u) -ne 0 || -z "${2:-}" || $(id -u "$2") -eq 0 ]]; then
    printf '%s\n' "System-delegated PTY acceptance requires root invocation and a non-root service account" >&2
    exit 1
  fi
  if [[ "$(id -Gn "$2")" != "$(id -gn "$2")" ]]; then
    printf '%s\n' "System-delegated PTY acceptance must not use an account with supplementary groups" >&2
    exit 1
  fi
  if [[ -z "${3:-}" || $(id -u "$3") -eq 0 || $(id -u "$3") -eq $(id -u "$2") ]]; then
    printf '%s\\n' "Distinct non-root shell UID is required for production acceptance" >&2
    exit 1
  fi
  # Optional 4th argument: a flowerlyd binary directory. The delegated unit
  # then runs the #74 daemon crash/restart matrix instead of the guardian
  # acceptance. It must be absolute and not writable by the manager account.
  daemon_bin="${4:-}"
  if [[ -n "$daemon_bin" ]]; then
    if [[ "$daemon_bin" != /* || ! -x "$daemon_bin/flowerlyd" ]] \
        || runuser -u "$2" -- test -w "$daemon_bin"; then
      printf '%s\n' "Daemon matrix needs an absolute, manager-read-only flowerlyd directory" >&2
      exit 1
    fi
  fi
  exec systemd-run --system --wait --collect --pipe \
    --property=Delegate=yes --property="User=$2" \
    --property="AmbientCapabilities=CAP_SETUID CAP_SETGID" \
    --property=NoNewPrivileges=no \
    --setenv="FLOWERLY_PTY_SHELL_UID=$(id -u "$3")" \
    --setenv="FLOWERLY_PTY_DAEMON_BIN=$daemon_bin" \
    --property="WorkingDirectory=$PWD" \
    /bin/bash "$PWD/scripts/arch-pty-cgroup-acceptance.sh" --delegated
fi

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
  write_management_pid "$FLOWERLY_PTY_CGROUP_ROOT"
  printf '%s' '+cpu +memory +pids' > "$FLOWERLY_PTY_CGROUP_ROOT/cgroup.subtree_control"
fi

export FLOWERLY_PTY_ISOLATION=required
if [[ -n "${FLOWERLY_PTY_DAEMON_BIN:-}" ]]; then
  exec python3 scripts/test-pty-daemon-recovery.py "$FLOWERLY_PTY_DAEMON_BIN"
fi
python3 scripts/test-pty-cgroup.py

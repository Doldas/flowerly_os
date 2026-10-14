#!/usr/bin/env bash
set -euo pipefail

readonly test_user=flowerly-pty-ci

if [[ "${1:-}" == "root-refusal" ]]; then
  if [[ $(id -u) -ne 0 ]]; then
    exec sudo -n -- python3 scripts/test-pty-guardian.py --expect-root-denied
  fi
  exec python3 scripts/test-pty-guardian.py --expect-root-denied
fi

if [[ $(id -u) -ne 0 ]]; then
  case "${1:-}" in
    prepare)
      printf 'Unprivileged hosted PTY test identity: %s\n' "$(id)"
      test -r "$PWD/scripts/pty-guardian.py"
      test -x "$PWD/target/release/sessiond"
      test -x "$PWD/target/release/flowd"
      ;;
    run)
      shift
      if [[ "$#" -eq 0 ]]; then
        printf '%s\n' "Expected a PTY test command" >&2
        exit 2
      fi
      exec env -i "HOME=$HOME" "PATH=/usr/local/bin:/usr/bin:/bin" "LANG=C.UTF-8" "$@"
      ;;
    *)
      printf '%s\n' "Unprivileged runners may prepare or run PR tests, but cannot launch release system delegation" >&2
      exit 2
      ;;
  esac
  exit 0
fi

if ! id "$test_user" >/dev/null 2>&1; then
  useradd --system --user-group --create-home \
    --home-dir /var/lib/flowerly-pty-ci --shell /usr/sbin/nologin "$test_user"
fi

test_uid="$(id -u "$test_user")"
test_groups="$(id -Gn "$test_user")"
if [[ "$test_uid" -eq 0 || "$test_groups" != "$test_user" ]]; then
  printf '%s\n' "Dedicated PTY test user must have non-root UID and no supplementary groups" >&2
  exit 1
fi

test_home="$(getent passwd "$test_user" | cut -d: -f6)"

case "${1:-}" in
  prepare)
    printf 'Dedicated PTY test identity: %s\n' "$(id "$test_user")"
    for path in "$PWD/scripts/pty-guardian.py" "$PWD/target/release/sessiond" "$PWD/target/release/flowd"; do
      access=-x
      if [[ "$path" == */pty-guardian.py ]]; then
        access=-r
      fi
      if ! runuser -u "$test_user" -- test "$access" "$path"; then
        printf 'Dedicated PTY test user cannot access: %s\n' "$path" >&2
        namei -l "$path" >&2 || true
        exit 1
      fi
    done
    ;;
  run)
    shift
    if [[ "$#" -eq 0 ]]; then
      printf '%s\n' "Expected a command to run as the dedicated PTY user" >&2
      exit 2
    fi
    exec runuser -u "$test_user" -- env -i \
      "HOME=$test_home" "PATH=/usr/local/bin:/usr/bin:/bin" "LANG=C.UTF-8" "$@"
    ;;
  delegated)
    exec bash ./scripts/arch-pty-cgroup-acceptance.sh --system "$test_user"
    ;;
  *)
    printf '%s\n' "usage: ci-pty-user.sh {prepare|root-refusal|run COMMAND...|delegated}" >&2
    exit 2
    ;;
esac

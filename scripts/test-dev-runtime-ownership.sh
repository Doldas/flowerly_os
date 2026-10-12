#!/usr/bin/env bash
# Regression: stopping one Flowerly runtime must not signal another runtime.
set -euo pipefail
tmp="$(mktemp -d)"
owned_pid=""
other_pid=""
cleanup() {
    [[ -z "$owned_pid" ]] || kill "$owned_pid" 2>/dev/null || true
    [[ -z "$other_pid" ]] || kill "$other_pid" 2>/dev/null || true
    [[ -z "$owned_pid" ]] || wait "$owned_pid" 2>/dev/null || true
    [[ -z "$other_pid" ]] || wait "$other_pid" 2>/dev/null || true
    rm -rf "$tmp"
}
trap cleanup EXIT
mkdir -p "$tmp/bin" "$tmp/a" "$tmp/b"
cp "$(command -v sleep)" "$tmp/bin/agenticd"
AGENTIC_RUNTIME_DIR="$tmp/a" "$tmp/bin/agenticd" 30 &
owned_pid=$!
AGENTIC_RUNTIME_DIR="$tmp/b" "$tmp/bin/agenticd" 30 &
other_pid=$!
for _ in $(seq 1 20); do
    [[ -r "/proc/$owned_pid/stat" && -r "/proc/$other_pid/stat" ]] && break
    sleep 0.05
done
line="$(cat "/proc/$owned_pid/stat")"
line="${line##*) }"
ticks="$(awk '{print $20}' <<< "$line")"
printf '%s %s\n' "$owned_pid" "$ticks" > "$tmp/a/agenticd.pid"
RUNTIME_DIR="$tmp/a" STATE_DIR="$tmp/state" BIN_DIR="$tmp/bin" \
    bash "$(dirname "$0")/dev-runtime.sh" stop
kill -0 "$other_pid" 2>/dev/null || { echo "unrelated process was killed" >&2; exit 1; }
echo "runtime ownership isolation ok"

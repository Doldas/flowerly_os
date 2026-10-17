#!/usr/bin/env bash

set -euo pipefail

BIN_DIR="${1:-target/release}"
SUPERVISOR="$BIN_DIR/flowerlyd"
CTL="$BIN_DIR/agenticctl"

for binary in "$SUPERVISOR" "$CTL"; do
    if [[ ! -x "$binary" ]]; then
        echo "missing executable: $binary" >&2
        exit 1
    fi
done

ROOT="$(mktemp -d)"
RUNTIME_DIR="$ROOT/runtime"
STATE_DIR="$ROOT/state"
mkdir -p "$RUNTIME_DIR" "$STATE_DIR"

pid=""

cleanup() {
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -TERM "$pid" 2>/dev/null || true
        for _ in $(seq 1 30); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
    fi
    rm -rf "$ROOT"
}
trap cleanup EXIT

AGENTIC_RUNTIME_DIR="$RUNTIME_DIR" AGENTIC_STATE_DIR="$STATE_DIR" "$SUPERVISOR" >"$ROOT/flowerlyd.log" 2>&1 &
pid=$!

ready=0
for _ in $(seq 1 100); do
    if "$CTL" --runtime-dir "$RUNTIME_DIR" ping flowerlyd >/dev/null 2>&1; then
        ready=1
        break
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "flowerlyd exited before readiness" >&2
        cat "$ROOT/flowerlyd.log" >&2 || true
        exit 1
    fi
    sleep 0.1
done

if [[ "$ready" -ne 1 ]]; then
    echo "flowerlyd did not become ready" >&2
    cat "$ROOT/flowerlyd.log" >&2 || true
    exit 1
fi

default_json="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal create "Default priority")"
explicit_json="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal create "Explicit priority" --priority 7)"

python3 - "$default_json" "$explicit_json" <<'PY'
import json
import sys

default = json.loads(sys.argv[1])
explicit = json.loads(sys.argv[2])

for label, value in (("default", default), ("explicit", explicit)):
    if not isinstance(value.get("id"), str) or not value["id"].startswith("goal-"):
        raise SystemExit(f"{label} goal did not return a typed goal id: {value}")
    if value.get("objective") not in ("Default priority", "Explicit priority"):
        raise SystemExit(f"{label} goal objective mismatch: {value}")
PY

# Gate A: the invalid-priority CLI case must be refused with a machine-
# readable typed error, never a panic or silent success.
invalid_json="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal create "Invalid priority" --priority not-a-number 2>&1)" && {
    echo "invalid priority was accepted: $invalid_json" >&2
    exit 1
}
python3 - "$invalid_json" <<'PY'
import json
import sys

raw = sys.argv[1]
try:
    payload = json.loads(raw)
except ValueError:
    payload = raw
if isinstance(payload, dict):
    code = payload.get("code") or (payload.get("data") or {}).get("code")
    if code != "invalid_argument":
        raise SystemExit(f"invalid priority not typed invalid_argument: {payload}")
else:
    if "invalid" not in str(payload).lower():
        raise SystemExit(f"invalid priority output lacks typed error: {payload}")
PY

echo "CLI-to-flowerlyd create contract passed with omitted, explicit and refused invalid priority."

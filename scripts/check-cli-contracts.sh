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

# Start flowerlyd with the given extra environment and wait for readiness.
start_flowerlyd() {
    env AGENTIC_RUNTIME_DIR="$RUNTIME_DIR" AGENTIC_STATE_DIR="$STATE_DIR" "$@" \
        "$SUPERVISOR" >"$ROOT/flowerlyd.log" 2>&1 &
    pid=$!

    local ready=0
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
}

stop_flowerlyd() {
    kill -TERM "$pid"
    wait "$pid" || true
    pid=""
}

start_flowerlyd

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

parent_id="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["id"])' "$default_json")"
child_json="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal child "$parent_id" "Child objective")"
missing_child="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal child goal-00000000000000000000000000000000 "Orphan" 2>&1)" && {
    echo "child of a missing Goal was accepted: $missing_child" >&2
    exit 1
}
python3 - "$child_json" "$parent_id" "$missing_child" <<'PY'
import json
import sys

child, parent, missing = json.loads(sys.argv[1]), sys.argv[2], sys.argv[3]
if not child.get("id", "").startswith("goal-") or child.get("objective") != "Child objective":
    raise SystemExit(f"goal child returned no typed child Goal: {child}")
if "NotFound" not in missing:
    raise SystemExit(f"child of a missing Goal not refused as NotFound: {missing}")
PY
shown_parent="$("$CTL" --runtime-dir "$RUNTIME_DIR" goal show "$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["id"])' "$child_json")")"
python3 -c 'import json,sys; g=json.loads(sys.argv[1]); assert g["parent_goal"] == sys.argv[2], g' "$shown_parent" "$parent_id"

# With no admin group configured every Admin command is refused, so the CLI
# must surface a typed permission error and a non-zero exit, never success.
generation_id="$("$CTL" --runtime-dir "$RUNTIME_DIR" generation create | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"
for argv in \
    "generation promote $generation_id --reason cli-contract" \
    "generation evidence $generation_id --outcome passed --evidence-sha256 $(printf '9%.0s' {1..64}) --reason cli-contract" \
    "generation test $generation_id --experiment exp-x --reason cli-contract" \
    "generation reject $generation_id --reason cli-contract" \
    "generation reject $generation_id --experiment exp-x --reason cli-contract" \
    "generation revoke $generation_id --reason cli-contract" \
    "generation activate $generation_id --reason cli-contract" \
    "generation rollback $generation_id --reason cli-contract" \
    "generation history" \
    "experiment inspect exp-x" \
    "experiment list" \
    "experiment list --candidate $generation_id --limit 5" \
    "ownership inspect $parent_id --reason cli-contract" \
    "ownership claim $parent_id --to-uid 4242 --reason cli-contract --expect 2" \
    "ownership release $parent_id --reason cli-contract --expect 2"; do
    # shellcheck disable=SC2086
    if refused="$("$CTL" --runtime-dir "$RUNTIME_DIR" $argv 2>&1)"; then
        echo "admin command succeeded without an admin group: agenticctl $argv: $refused" >&2
        exit 1
    fi
    if [[ "$refused" != *PermissionDenied* ]]; then
        echo "admin command not refused as PermissionDenied: agenticctl $argv: $refused" >&2
        exit 1
    fi
done
# A malformed --expect never reaches the daemon.
if "$CTL" --runtime-dir "$RUNTIME_DIR" ownership claim "$parent_id" --to-uid 1 --reason x --expect -1 >/dev/null 2>&1; then
    echo "negative --expect was accepted" >&2
    exit 1
fi

# An unreadable or non-object experiment file never reaches the daemon.
evidence_sha="$(printf '9%.0s' {1..64})"
printf '[]' >"$ROOT/not-object.json"
for file in "$ROOT/missing.json" "$ROOT/not-object.json"; do
    if local_error="$("$CTL" --runtime-dir "$RUNTIME_DIR" generation evidence "$generation_id" \
        --outcome passed --evidence-sha256 "$evidence_sha" --experiment-file "$file" \
        --reason cli-contract 2>&1)"; then
        echo "a bad experiment file was accepted: $file" >&2
        exit 1
    fi
    if [[ "$local_error" == *PermissionDenied* || "$local_error" != *"experiment file"* ]]; then
        echo "a bad experiment file was not refused by the CLI: $local_error" >&2
        exit 1
    fi
done

# With the invoking user's group as the admin group, an Evolution Lab record
# goes through the CLI end to end (ADR-0024 D5): evidence with details, read
# back, decided by test or reject.
stop_flowerlyd
start_flowerlyd AGENTIC_ADMIN_GROUP="$(id -g)"
ctl() {
    "$CTL" --runtime-dir "$RUNTIME_DIR" "$@"
}
json_field() {
    python3 -c 'import json,sys; print(json.loads(sys.argv[1])[sys.argv[2]])' "$1" "$2"
}
baseline_id="$(json_field "$(ctl generation create)" id)"
good_id="$(json_field "$(ctl generation create --parent "$baseline_id")" id)"
bad_id="$(json_field "$(ctl generation create --parent "$baseline_id")" id)"
for id in "$baseline_id" "$good_id" "$bad_id"; do
    ctl generation promote "$id" --reason cli-contract >/dev/null
done
ctl generation identity "$good_id" --source-commit "$(printf 'a%.0s' {1..40})" \
    --rootfs-sha256 "$(printf 'b%.0s' {1..64})" --kernel-sha256 "$(printf 'c%.0s' {1..64})" \
    --initramfs-sha256 "$(printf 'd%.0s' {1..64})" \
    --input-manifest-sha256 "$(printf 'e%.0s' {1..64})" --build-id cli-contract \
    --reason cli-contract >/dev/null
python3 - "$ROOT/experiment.json" "$baseline_id" <<'PY'
import json
import sys

path, baseline = sys.argv[1], sys.argv[2]
details = {
    "hypothesis": "the candidate boots and answers",
    "baseline": baseline,
    "test_plan": ["identity.root", "health.flowerlyd"],
    "metrics": {"boot.report_ms": 8125.0},
    "artifacts": [
        {"kind": "result", "name": "result.json", "sha256": "f" * 64, "bytes": 2048}
    ],
    "lane": "reproducibility",
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(details, handle)
PY
good_evidence="$(ctl generation evidence "$good_id" --outcome passed \
    --evidence-sha256 "$evidence_sha" --experiment-file "$ROOT/experiment.json" \
    --reason cli-contract)"
good_experiment="$(json_field "$good_evidence" experiment)"
ctl generation test "$good_id" --experiment "$good_experiment" --reason cli-contract >/dev/null
bad_evidence="$(ctl generation evidence "$bad_id" --outcome failed \
    --evidence-sha256 "$(printf '8%.0s' {1..64})" --experiment-file "$ROOT/experiment.json" \
    --reason cli-contract)"
bad_experiment="$(json_field "$bad_evidence" experiment)"
if wrong="$(ctl generation reject "$good_id" --experiment "$bad_experiment" --reason x 2>&1)"; then
    echo "a rejection accepted another Generation's Experiment: $wrong" >&2
    exit 1
fi
ctl generation reject "$bad_id" --experiment "$bad_experiment" --reason cli-contract >/dev/null
python3 - "$(ctl experiment inspect "$good_experiment")" "$(ctl experiment inspect "$bad_experiment")" \
    "$(ctl experiment list --candidate "$bad_id")" "$baseline_id" "$good_id" <<'PY'
import json
import sys

good, bad, listed = (json.loads(arg) for arg in sys.argv[1:4])
baseline, good_id = sys.argv[4], sys.argv[5]
assert good["candidate"] == good_id and good["baseline"] == baseline, good
assert (good["result"], good["decision"]) == ("passed", "tested"), good
assert (bad["result"], bad["decision"]) == ("failed", "rejected"), bad
assert good["test_plan"] == ["identity.root", "health.flowerlyd"], good
assert good["metrics"]["values"] == {"boot.report_ms": 8125.0}, good
assert good["metrics"]["lane"] == "reproducibility", good
assert [a["uri"] for a in good["artifacts"]] == ["sha256:" + "f" * 64], good
assert listed["count"] == 1 and listed["experiments"][0]["id"] == bad["id"], listed
PY

echo "CLI-to-flowerlyd create contract passed with omitted, explicit and refused invalid priority."
echo "CLI goal child creates a linked child and refuses a missing parent; Admin generation promote and ownership commands are refused without an admin group."
echo "With an admin group, an Evolution Lab Experiment is recorded, decided and read back through agenticctl."

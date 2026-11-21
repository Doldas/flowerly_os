#!/usr/bin/env bash
# `just vm-test` / `make vm-test` (ADR-0024, #412): build an Evolution Lab
# candidate from the cached pinned inputs, boot it on this KVM host with
# `evolutiond lab run --record`, and record the result in a private lab
# flowerlyd. Runs in the foreground as the invoking user; nothing is
# activated, and the host's own flowerlyd (if any) is never contacted.
#
# usage: vm-test.sh [--scenario good|bad|rollback] [--tries N]
#   good      B healthy: the run passes and the candidate becomes TESTED
#   bad       B's entry injects a health failure: the candidate is REJECTED
#   rollback  as bad, booted once per boot-count try, then the loader must
#             fall back to A, which reports healthy: REJECTED with a fallback
#
# Inputs (no network is used; build missing inputs first):
#   FLOWERLY_LAB_ROOTFS     pinned rootfs (target/pinned-rootfs/flowerly-rootfs,
#                           from scripts/build-arch-rootfs-pinned-container.sh)
#   FLOWERLY_LAB_INITRAMFS  initramfs (target/pinned-rootfs/initramfs.img,
#                           from scripts/build-arch-initrd.sh)
#   FLOWERLY_LAB_CANDIDATE  reuse a candidate built at this commit
#   FLOWERLY_LAB_OUT        new output directory (target/lab/<utc>-<scenario>)
#   FLOWERLY_LAB_OVMF_CODE, FLOWERLY_LAB_OVMF_VARS  OVMF firmware and vars
#   FLOWERLY_LAB_QEMU       QEMU binary (qemu-system-x86_64 from PATH)
#
# Each run boots its own copy of the built disk: first once as slot A,
# unrecorded (the machine's first boot, which the guest requires before any
# boot of B), then, with B armed, the recorded run.
#
# Exit status: 0 when the recorded outcome is the scenario's (for good, a
# recorded passing run: TESTED), 3 when the other outcome was recorded,
# 1 on any error, 2 on usage.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fail() { echo "vm-test: $*" >&2; exit 1; }
usage() { echo "usage: $0 [--scenario good|bad|rollback] [--tries N]" >&2; exit 2; }
SCENARIO=good; TRIES=2
while (( $# )); do
    case "$1" in
        --scenario) (( $# >= 2 )) || usage; SCENARIO="$2"; shift 2 ;;
        --tries) (( $# >= 2 )) || usage; TRIES="$2"; shift 2 ;;
        *) usage ;;
    esac
done
case "$SCENARIO" in good) WANT=TESTED ;; bad|rollback) WANT=REJECTED ;; *) usage ;; esac
[[ "$TRIES" =~ ^[1-9]$ ]] || usage

[[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] || fail "/dev/kvm is not usable by $(id -un)"
QEMU="${FLOWERLY_LAB_QEMU:-$(command -v qemu-system-x86_64 || true)}"
[[ -x "$QEMU" ]] || fail "missing qemu-system-x86_64 (or FLOWERLY_LAB_QEMU)"
firmware() { # variable candidates...
    local name="$1" path; shift
    if [[ -n "${!name:-}" ]]; then echo "${!name}"; return; fi
    for path in "$@"; do [[ -f "$path" ]] && { echo "$path"; return; }; done
    fail "no OVMF firmware found; set $name"
}
CODE="$(firmware FLOWERLY_LAB_OVMF_CODE /usr/share/edk2/x64/OVMF_CODE.4m.fd \
    /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/edk2/ovmf/OVMF_CODE.fd)"
VARS="$(firmware FLOWERLY_LAB_OVMF_VARS /usr/share/edk2/x64/OVMF_VARS.4m.fd \
    /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/edk2/ovmf/OVMF_VARS.fd)"

COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
OUT="${FLOWERLY_LAB_OUT:-$ROOT/target/lab/$(date -u +%Y%m%dT%H%M%SZ)-$SCENARIO}"
[[ ! -e "$OUT" ]] || fail "refusing to reuse $OUT"
mkdir -p "$(dirname "$OUT")"; mkdir -m 0700 "$OUT"; OUT="$(realpath "$OUT")"
echo "vm-test: $SCENARIO at $COMMIT in $OUT"

cargo build --release --locked -p flowerlyd -p agenticctl -p evolutiond -p distro-builder
BIN="$ROOT/target/release"
json() { python3 -c 'import json, sys
value = json.load(open(sys.argv[1]))
for key in sys.argv[2:]:
    value = value[key]
print(value)' "$@"; }

if [[ -n "${FLOWERLY_LAB_CANDIDATE:-}" ]]; then
    CANDIDATE="$(realpath "$FLOWERLY_LAB_CANDIDATE")"
    [[ "$(json "$CANDIDATE/candidate.json" source_commit)" == "$COMMIT" ]] ||
        fail "$CANDIDATE was not built at $COMMIT"
else
    ROOTFS="${FLOWERLY_LAB_ROOTFS:-$ROOT/target/pinned-rootfs/flowerly-rootfs}"
    INITRAMFS="${FLOWERLY_LAB_INITRAMFS:-$ROOT/target/pinned-rootfs/initramfs.img}"
    [[ -d "$ROOTFS" ]] || fail "missing pinned rootfs $ROOTFS
  build it: bash scripts/build-arch-rootfs-pinned-container.sh target/pinned-rootfs
  or set FLOWERLY_LAB_ROOTFS"
    [[ -f "$INITRAMFS" ]] || fail "missing initramfs $INITRAMFS
  build it with scripts/build-arch-initrd.sh (see web-docs/docs/build.md)
  or set FLOWERLY_LAB_INITRAMFS"
    CANDIDATE="$OUT/candidate"
    nice bash "$ROOT/scripts/build-lab-candidate.sh" "$ROOTFS" "$INITRAMFS" "$CANDIDATE"
fi

# A private flowerlyd whose only Admin is the invoking user: its socket is
# 0600 in a 0700 runtime directory, and the Admin group is the user's own.
LAB="$OUT/flowerlyd"; mkdir -m 0700 "$LAB" "$LAB/runtime" "$LAB/state"
env AGENTIC_RUNTIME_DIR="$LAB/runtime" AGENTIC_STATE_DIR="$LAB/state" \
    AGENTIC_ADMIN_GROUP="$(id -g)" "$BIN/flowerlyd" > "$LAB/flowerlyd.log" 2>&1 &
DAEMON=$!
trap 'kill "$DAEMON" 2>/dev/null || true; wait "$DAEMON" 2>/dev/null || true' EXIT
ctl() { "$BIN/agenticctl" --runtime-dir "$LAB/runtime" "$@"; }
for _ in $(seq 100); do
    ctl ping >/dev/null 2>&1 && break
    kill -0 "$DAEMON" 2>/dev/null || fail "the lab flowerlyd exited: $(tail -5 "$LAB/flowerlyd.log")"
    sleep 0.1
done
ctl ping >/dev/null || fail "the lab flowerlyd did not answer"

# Each slot's Generation, with the root identity its slot was built with: the
# build_id is what ties the guest's report to it.
generation() { # slot
    local created id
    created="$(ctl generation create --source-commit "$COMMIT")"
    id="$(python3 -c 'import json, sys; print(json.loads(sys.argv[1])["id"])' "$created")"
    ctl generation identity "$id" --source-commit "$COMMIT" \
        --rootfs-sha256 "$(json "$CANDIDATE/candidate.json" slots "$1" rootfs_sha256)" \
        --kernel-sha256 "$(json "$CANDIDATE/candidate.json" kernel_sha256)" \
        --initramfs-sha256 "$(json "$CANDIDATE/candidate.json" initramfs_sha256)" \
        --input-manifest-sha256 "$(json "$CANDIDATE/candidate.json" input_manifest_sha256)" \
        --build-id "$(json "$CANDIDATE/candidate.json" slots "$1" build_id)" \
        --reason "evolution lab vm-test: $1 as built at $COMMIT" >/dev/null
    echo "$id"
}
BASELINE="$(generation root-a)"; CANDIDATE_ID="$(generation root-b)"
echo "vm-test: baseline $BASELINE (root-a), candidate $CANDIDATE_ID (root-b)"

python3 "$ROOT/scripts/lab-candidate.py" prepare "$CANDIDATE" "$OUT/run" \
    --scenario "$SCENARIO" --tries "$TRIES" \
    --candidate-generation "$CANDIDATE_ID" --baseline-generation "$BASELINE" \
    --firmware-code "$CODE" --firmware-vars "$VARS" \
    --evolutiond "$BIN/evolutiond" --qemu "$QEMU"
STATUS=0
"$BIN/evolutiond" lab run --plan "$OUT/run/plan.json" --out "$OUT/run/result" \
    --qemu "$QEMU" --record --runtime-dir "$LAB/runtime" || STATUS=$?
(( STATUS == 0 || STATUS == 3 )) || fail "evolutiond lab run failed ($STATUS); see $OUT"

GOT="$(ctl generation inspect "$CANDIDATE_ID" | python3 -c 'import json, sys; print(json.load(sys.stdin)["status"])')"
echo "vm-test: $SCENARIO: candidate $CANDIDATE_ID is $GOT (expected $WANT)"
[[ "$GOT" == "$WANT" ]] || exit 3
if [[ "$SCENARIO" == rollback ]]; then
    FALLBACK="$(python3 -c 'import json, sys
f = json.load(open(sys.argv[1]))["decision"]["fallback"]
print("yes" if f and f["booted"] and f["healthy"] else "no")' "$OUT/run/result/result.json")"
    echo "vm-test: rollback: healthy fallback to root-a observed: $FALLBACK"
    [[ "$FALLBACK" == yes ]] || exit 3
fi
echo "vm-test: PASS ($SCENARIO) evidence $(cut -d' ' -f1 "$OUT/run/result/result.sha256")"

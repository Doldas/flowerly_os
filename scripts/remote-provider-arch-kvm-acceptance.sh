#!/usr/bin/env bash
# Remote provider Arch Linux/KVM security acceptance harness (#263, ADR-0022).
#
#   remote-provider-arch-kvm-acceptance.sh --help
#   remote-provider-arch-kvm-acceptance.sh --plan
#   remote-provider-arch-kvm-acceptance.sh --validate [--evidence-out FILE]
#   remote-provider-arch-kvm-acceptance.sh --dry-run
#   remote-provider-arch-kvm-acceptance.sh --execute --approved-sha <40-hex sha>
#
# Phases are separate and only --execute ever starts anything:
#   preparation  --plan, --validate, --dry-run: read the repository, check
#                scripts and configuration, print what an execution would do.
#                They never start QEMU, download, build, contact a network,
#                use sudo or change the host, and the only evidence they can
#                write is NOT_RUN.
#   execution    --execute: builds the exact commit, verifies the pinned
#                image, boots ONE disposable guest under KVM (never TCG), with
#                no route to the host or the internet, and runs
#                testbed/remote-provider-kvm/guest/guest-acceptance.py in it.
#   evidence     after execution: the guest's evidence document is copied
#                out, validated (scripts/remote-provider-kvm-evidence.py) and
#                kept; the guest, its disk and its keys are destroyed.
#
# --execute refuses unless ALL of these hold; each refusal happens before any
# download, build or VM:
#   - FLOWERLY_KVM_ACCEPTANCE=execute-in-disposable-guest is set;
#   - --approved-sha equals HEAD, and the working tree is clean;
#   - not root, not a CI runner (GITHUB_ACTIONS/CI unset);
#   - testbed/remote-provider-kvm/arch-image.json is pinned (version, URL,
#     SHA-256, signature URL, signing-key fingerprint) and the keyring in
#     FLOWERLY_KVM_ARCH_KEYRING holds exactly that key;
#   - /dev/kvm is usable and every host tool is present.
# These are guardrails against accidents. They are NOT evidence that the
# owner approved a run: that approval is recorded separately (see
# web-docs/docs/remote-provider-unit-test-plan.md, "Authorization").
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TESTBED="$ROOT/testbed/remote-provider-kvm"
EVIDENCE_TOOL="$ROOT/scripts/remote-provider-kvm-evidence.py"
EXECUTE_TOKEN=execute-in-disposable-guest
HOST_TOOLS=(git python3 cargo curl gpg sha256sum qemu-img qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar)
VM_CPUS=4
VM_MEMORY_MB=4096
VM_DISK_GB=16

die() { echo "remote-provider-kvm: $*" >&2; exit 1; }
say() { echo "remote-provider-kvm: $*"; }
usage() { sed -n '4,8p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,3\}//'; }

head_sha() { git -C "$ROOT" rev-parse HEAD; }
tree_clean() { [[ -z $(git -C "$ROOT" status --porcelain --untracked-files=normal) ]]; }
tree_state() { if tree_clean; then echo clean; else echo dirty; fi; }

# --- preparation (no side effects) -----------------------------------------

# Prints `pinned` or the reason it is not; exits nonzero only for a
# malformed lock file. Never fetches anything.
image_lock_state() {
    python3 -I - "$TESTBED/arch-image.json" <<'EOF'
import json, re, sys
lock = json.load(open(sys.argv[1]))
fields = ["version", "url", "sha256", "signature_url", "signing_key_fingerprint", "pinned_by", "pinned_at_utc"]
missing = [f for f in fields if lock.get(f) in (None, "")]
if set(lock) - set(fields) - {"schema_version", "comment", "source_index"}:
    sys.exit("arch-image.json has unknown fields")
if missing:
    print("UNPINNED: " + ", ".join(missing) + " not set")
    sys.exit(0)
version = lock["version"]
problems = []
if not re.fullmatch(r"\d{8}\.\d+", version):
    problems.append("version must look like 20261001.123456")
base = f"https://geo.mirror.pkgbuild.com/images/v{version}/"
if not lock["url"].startswith(base) or "latest" in lock["url"] or not lock["url"].endswith(".qcow2"):
    problems.append(f"url must be a versioned .qcow2 under {base}")
if lock["signature_url"] != lock["url"] + ".sig":
    problems.append("signature_url must be url + .sig")
if not re.fullmatch(r"[0-9a-f]{64}", lock["sha256"]):
    problems.append("sha256 must be 64 lowercase hex")
if not re.fullmatch(r"[0-9A-F]{40}", lock["signing_key_fingerprint"]):
    problems.append("signing_key_fingerprint must be 40 uppercase hex")
if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", lock["pinned_at_utc"]):
    problems.append("pinned_at_utc must be UTC")
if problems:
    sys.exit("arch-image.json: " + "; ".join(problems))
print("pinned")
EOF
}

validate() {
    local status=0 file
    say "commit $(head_sha), tree $(tree_state)"
    for file in "$ROOT/scripts/remote-provider-arch-kvm-acceptance.sh" "$TESTBED/guest/make-test-pki.sh" \
                "$TESTBED/guest/interpose.sh" "$ROOT/scripts/remote-provider-operator.sh"; do
        bash -n "$file" || { echo "syntax error: $file" >&2; status=1; }
    done
    for file in "$TESTBED"/guest/*.py "$ROOT/scripts/provider-unit-probe.py" "$EVIDENCE_TOOL" \
                "$ROOT/scripts/remote-provider-acceptance.py"; do
        python3 -I -c 'import ast, sys; ast.parse(open(sys.argv[1]).read(), sys.argv[1])' "$file" \
            || { echo "syntax error: $file" >&2; status=1; }
    done
    for file in "$TESTBED"/*.json; do
        python3 -I -m json.tool "$file" >/dev/null || { echo "invalid JSON: $file" >&2; status=1; }
    done
    local image
    image=$(image_lock_state) || { echo "$image" >&2; status=1; }
    say "image lock: $image"
    # A NOT_RUN document must be producible and valid; nothing else can be.
    local scratch
    scratch=$(mktemp -d)
    python3 -I "$EVIDENCE_TOOL" not-run --commit "$(head_sha)" --clean "$(tree_clean && echo yes || echo no)" \
        --reason "preparation only: the Arch/KVM acceptance has not been executed" \
        --out "$scratch/not-run.json" >/dev/null || status=1
    python3 -I "$EVIDENCE_TOOL" validate "$scratch/not-run.json" --phase preparation >/dev/null || status=1
    rm -rf "$scratch"
    if [[ $image != pinned ]]; then
        say "execution readiness: BLOCKED (image not pinned)"
    else
        say "execution readiness: prepared; execution still needs the owner's authorization"
    fi
    say "Arch/KVM acceptance status: NOT_RUN"
    return $status
}

plan() {
    cat <<EOF
Remote provider Arch/KVM security acceptance (#263) - PLAN ONLY, nothing runs.
commit:        $(head_sha)
working tree:  $(tree_state)
image lock:    $(image_lock_state)
status:        NOT_RUN

preparation  --validate / --dry-run: syntax, configuration, evidence rules. No VM.
execution    --execute --approved-sha <sha> with FLOWERLY_KVM_ACCEPTANCE=$EXECUTE_TOKEN:
  1. refuse on: wrong SHA, dirty tree, root, CI runner, unpinned image, missing
     keyring, unusable /dev/kvm, missing host tool
  2. export the commit (git archive) into a fresh run directory; cargo build
     --release --locked --offline -p providerd -p flowerlyd there
  3. download the pinned image (exact versioned URL) and its signature; verify
     SHA-256 and the signature against the pinned key; never 'latest'
  4. boot one guest: -accel kvm (no TCG fallback), -nic user,restrict=on with
     only an SSH port forward on 127.0.0.1, qcow2 overlay, unique run id
  5. copy the bundle (export, binaries, manifest, run facts); run
     guest-acceptance.py as root INSIDE the guest
evidence     copy out and validate the guest's evidence; destroy guest, overlay,
             seed and SSH key; keep only the report directory
EOF
}

dry_run() {
    validate
    local run_id
    run_id="dryrun-$(head_sha | cut -c1-12)"
    cat <<EOF

Commands an execution WOULD run (dry run: none of them is executed):
  git -C $ROOT archive --format=tar <approved-sha> | tar -x -C <run>/export
  cargo build --release --locked --offline -p providerd -p flowerlyd   (in <run>/export)
  curl --fail --location -o <cache>/<image> <pinned url>   (+ .sig)
  sha256sum -c <pinned sha256>;  gpg --no-default-keyring --keyring <keyring> --verify <image>.sig <image>
  qemu-img create -f qcow2 -F qcow2 -b <image> <run>/overlay.qcow2 ${VM_DISK_GB}G
  cloud-localds <run>/seed.img <run>/user-data   (marker + hostname flowerly-kvm-<run id>, no packages)
  qemu-system-x86_64 $(qemu_args "$run_id" "<run>" "<image>" "<port>" | tr '\n' ' ')
  scp <bundle> guest:/opt/flowerly-kvm/ ; ssh guest sudo python3 -I .../guest-acceptance.py /opt/flowerly-kvm
  scp guest:/var/lib/flowerly-kvm/evidence/*.json <run>/report/
  python3 scripts/remote-provider-kvm-evidence.py validate <evidence> --phase execution
EOF
}

# The one QEMU command line, shared by --dry-run and --execute. KVM only.
qemu_args() {
    local run_id=$1 run=$2 image=$3 port=$4
    printf '%s\n' -name "flowerly-kvm-$run_id" -machine q35 -accel kvm -cpu host \
        -smp "$VM_CPUS" -m "$VM_MEMORY_MB" \
        -drive "if=virtio,format=qcow2,file=$run/overlay.qcow2" \
        -drive "if=virtio,format=raw,readonly=on,file=$run/seed.img" \
        -nic "user,model=virtio-net-pci,restrict=on,hostfwd=tcp:127.0.0.1:$port-:22" \
        -display none -serial "file:$run/report/serial.log" -monitor none -no-reboot \
        -daemonize -pidfile "$run/qemu.pid"
}

# --- execution (never reached from the preparation modes) -------------------

require_execution_gates() {
    local approved=$1 tool
    [[ ${FLOWERLY_KVM_ACCEPTANCE:-} == "$EXECUTE_TOKEN" ]] \
        || die "--execute needs FLOWERLY_KVM_ACCEPTANCE=$EXECUTE_TOKEN; refusing"
    [[ $approved =~ ^[0-9a-f]{40}$ ]] || die "--approved-sha must be a full 40-hex commit"
    [[ $approved == "$(head_sha)" ]] || die "HEAD $(head_sha) is not the approved commit $approved; refusing"
    tree_clean || die "working tree is dirty; refusing (no owner-approved dirty-tree procedure exists)"
    [[ $(id -u) -ne 0 ]] || die "refusing to run as root: the host needs no privilege"
    [[ -z ${GITHUB_ACTIONS:-} && -z ${CI:-} ]] || die "refusing on a CI runner: execution is a manual, approved step"
    [[ $(image_lock_state) == pinned ]] || die "image not pinned: $(image_lock_state)"
    [[ -n ${FLOWERLY_KVM_ARCH_KEYRING:-} && -f $FLOWERLY_KVM_ARCH_KEYRING ]] \
        || die "FLOWERLY_KVM_ARCH_KEYRING must name the keyring holding the pinned signing key"
    [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] || die "/dev/kvm is not usable; refusing (no TCG fallback)"
    for tool in "${HOST_TOOLS[@]}"; do
        command -v "$tool" >/dev/null || die "missing host tool: $tool"
    done
}

lock_field() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$TESTBED/arch-image.json" "$1"; }

free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

execute() {
    local approved=$1
    require_execution_gates "$approved"
    local run_id run cache image port key ssh_opts guest
    run_id="$(date -u +%Y%m%dt%H%M%S)-${approved:0:12}-$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
    run="$ROOT/target/remote-provider-kvm/$run_id"
    cache="${FLOWERLY_KVM_IMAGE_CACHE:-$HOME/.cache/flowerly/remote-provider-kvm}"
    [[ ! -e $run ]] || die "run directory exists: $run"
    mkdir -p "$run/report" "$run/export" "$cache"
    say "run $run_id at $approved (phase: execution)"

    # Build exactly the approved commit, from an export, without network.
    git -C "$ROOT" archive --format=tar "$approved" | tar -x -C "$run/export"
    (cd "$run/export" && cargo build --release --locked --offline -p providerd -p flowerlyd) \
        || die "offline build failed; run 'cargo fetch --locked' beforehand as a separate step"

    # The pinned image only, verified before use; never substituted.
    image="$cache/$(basename "$(lock_field url)")"
    if [[ ! -f $image ]]; then
        curl --fail --location --show-error --output "$image.part" "$(lock_field url)"
        mv "$image.part" "$image"
    fi
    curl --fail --location --show-error --output "$run/report/image.sig" "$(lock_field signature_url)"
    echo "$(lock_field sha256)  $image" | sha256sum --check --status || die "image SHA-256 mismatch; refusing"
    local fingerprints
    fingerprints=$(gpg --no-default-keyring --keyring "$FLOWERLY_KVM_ARCH_KEYRING" --with-colons --fingerprint \
        | awk -F: '$1 == "fpr" { print $10 }' | sort -u)
    [[ $fingerprints == "$(lock_field signing_key_fingerprint)" ]] \
        || die "keyring must hold exactly the pinned signing key"
    gpg --no-default-keyring --keyring "$FLOWERLY_KVM_ARCH_KEYRING" --verify "$run/report/image.sig" "$image" \
        2>"$run/report/image-signature.txt" || die "image signature does not verify; refusing"

    qemu-img create -q -f qcow2 -F qcow2 -b "$image" "$run/overlay.qcow2" "${VM_DISK_GB}G"
    key="$run/id_ed25519"
    ssh-keygen -q -t ed25519 -N "" -f "$key" -C "flowerly-kvm-$run_id"
    cat >"$run/user-data" <<EOF
#cloud-config
hostname: flowerly-kvm-${run_id:0:20}
growpart: {mode: auto, devices: ['/']}
resize_rootfs: true
users:
  - name: flowerly-kvm
    groups: [wheel]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys: ['$(cat "$key.pub")']
write_files:
  - path: /etc/flowerly-kvm-disposable-guest
    permissions: '0644'
    content: |
      $run_id
EOF
    cloud-localds "$run/seed.img" "$run/user-data"
    port=$(free_port)
    local -a args
    mapfile -t args < <(qemu_args "$run_id" "$run" "$image" "$port")
    printf '%s\n' "${args[@]}" >"$run/report/qemu-args.txt"

    cleanup() {
        local pid
        pid=$(cat "$run/qemu.pid" 2>/dev/null || true)
        [[ $pid =~ ^[0-9]+$ ]] && kill "$pid" 2>/dev/null || true
        rm -f "$run/overlay.qcow2" "$run/seed.img" "$key" "$key.pub" "$run/user-data"
        rm -rf "$run/export" "$run/bundle"
    }
    trap cleanup EXIT
    qemu-system-x86_64 "${args[@]}" || die "QEMU/KVM did not start; refusing (no TCG fallback)"

    ssh_opts=(-i "$key" -p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
              -o ConnectTimeout=10 -o BatchMode=yes)
    guest=flowerly-kvm@127.0.0.1
    local _
    for _ in $(seq 1 90); do
        ssh "${ssh_opts[@]}" "$guest" test -f /etc/flowerly-kvm-disposable-guest 2>/dev/null && break
        sleep 5
    done
    # shellcheck disable=SC2029 # $run_id is meant to expand here, on the host.
    ssh "${ssh_opts[@]}" "$guest" "cloud-init status --wait >/dev/null; test \"\$(cat /etc/flowerly-kvm-disposable-guest)\" = $run_id" \
        || die "guest never came up with this run's marker"

    # Bundle: the export, the two binaries, run facts and a manifest.
    local bundle="$run/bundle"
    mkdir -p "$bundle/bin"
    git -C "$ROOT" archive --format=tar --prefix=repo/ "$approved" | tar -x -C "$bundle"
    cp "$run/export/target/release/providerd" "$run/export/target/release/flowerlyd" "$bundle/bin/"
    python3 -I - "$bundle" "$run_id" "$approved" "$(lock_field version)" "$(lock_field sha256)" <<'EOF'
import hashlib, json, pathlib, sys, time
bundle, run_id, commit, version, image_sha = pathlib.Path(sys.argv[1]), *sys.argv[2:]
now = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
host = lambda status, expected, observed: {"status": status, "expected": expected, "observed": observed,
                                           "started": now, "finished": now}
facts = {"run_id": run_id, "git_commit": commit, "git_tree_clean": True,
         "guest_image": f"Arch-Linux-x86_64-cloudimg-{version}", "guest_image_sha256": image_sha,
         "host_results": {
             "E1": host("PASS", "-accel kvm, no TCG", {"accel": "kvm", "qemu_started": True}),
             "E3": host("PASS", "pinned SHA-256 and signature", {"sha256": "verified", "signature": "verified"})}}
(bundle / "run.json").write_text(json.dumps(facts, indent=2))
manifest = {str(p.relative_to(bundle)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(bundle.rglob("*")) if p.is_file() and p.name not in ("run.json", "manifest.json")}
(bundle / "manifest.json").write_text(json.dumps(manifest, indent=2))
EOF
    tar -C "$bundle" -cf - . | ssh "${ssh_opts[@]}" "$guest" "sudo mkdir -p /opt/flowerly-kvm && sudo tar -x -C /opt/flowerly-kvm"
    local guest_status=0
    ssh "${ssh_opts[@]}" "$guest" "sudo python3 -I /opt/flowerly-kvm/repo/testbed/remote-provider-kvm/guest/guest-acceptance.py /opt/flowerly-kvm" \
        >"$run/report/guest.log" 2>&1 || guest_status=$?

    # Evidence phase.
    scp -P "$port" -i "$key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes \
        "$guest:/var/lib/flowerly-kvm/evidence/*.json" "$run/report/" || die "no evidence came out of the guest"
    local -a found=("$run/report"/remote-provider-kvm-*.json)
    [[ ${#found[@]} -eq 1 && -f ${found[0]} ]] || die "expected exactly one evidence document from the guest"
    local evidence=${found[0]}
    python3 -I "$EVIDENCE_TOOL" validate "$evidence" --phase execution | tee "$run/report/validation.json"
    say "guest exit $guest_status; evidence kept in $run/report; guest destroyed"
    return $guest_status
}

[[ $# -ge 1 ]] || { usage >&2; exit 2; }
case $1 in
    --help|-h) usage ;;
    --plan) [[ $# -eq 1 ]] || { usage >&2; exit 2; }; plan ;;
    --validate)
        if [[ $# -eq 3 && $2 == --evidence-out ]]; then
            validate
            python3 -I "$EVIDENCE_TOOL" not-run --commit "$(head_sha)" --clean "$(tree_clean && echo yes || echo no)" \
                --reason "preparation only: the Arch/KVM acceptance has not been executed" --out "$3"
        else
            [[ $# -eq 1 ]] || { usage >&2; exit 2; }
            validate
        fi ;;
    --dry-run) [[ $# -eq 1 ]] || { usage >&2; exit 2; }; dry_run ;;
    --execute)
        [[ $# -eq 3 && $2 == --approved-sha ]] || die "usage: --execute --approved-sha <40-hex sha>"
        execute "$3" ;;
    *) usage >&2; exit 2 ;;
esac

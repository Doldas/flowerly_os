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
#                out; E1-E3 are finalized from host-only facts (QEMU's running
#                command line, the egress canary, the image verification);
#                everything is bound into one acceptance record that
#                `remote-provider-kvm-evidence.py verify-bound` re-derives.
#                Only its PASS verdict is an acceptance candidate. The guest,
#                its disk and its keys are destroyed.
#
# --execute refuses unless ALL of these hold; each refusal happens before any
# download, build or VM:
#   - FLOWERLY_KVM_ACCEPTANCE=execute-in-disposable-guest is set;
#   - --approved-sha equals HEAD, and the working tree is clean;
#   - not root, not a CI runner (GITHUB_ACTIONS/CI unset);
#   - testbed/remote-provider-kvm/arch-image.json is pinned (version, URL,
#     SHA-256, signature URL, primary-key fingerprint, approved signing
#     subkeys) and the keyring in FLOWERLY_KVM_ARCH_KEYRING holds exactly
#     that primary key with those signing subkeys
#     (scripts/arch-image-verify.py keyring);
#   - /dev/kvm is usable and every host tool is present.
# These are guardrails against accidents. They are NOT evidence that the
# owner approved a run: that approval is recorded separately (see
# web-docs/docs/remote-provider-unit-test-plan.md, "Authorization").
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TESTBED="$ROOT/testbed/remote-provider-kvm"
EVIDENCE_TOOL="$ROOT/scripts/remote-provider-kvm-evidence.py"
IMAGE_TOOL="$ROOT/scripts/arch-image-verify.py"
EXECUTE_TOKEN=execute-in-disposable-guest
HOST_TOOLS=(git python3 cargo curl gpg qemu-img qemu-system-x86_64 cloud-localds ssh scp ssh-keygen tar)
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
fields = ["version", "url", "sha256", "signature_url", "primary_key_fingerprint", "signing_subkey_fingerprints",
          "pinned_by", "pinned_at_utc"]
missing = [f for f in fields if lock.get(f) in (None, "", [])]
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
fpr = re.compile(r"[0-9A-F]{40}")
if not isinstance(lock["primary_key_fingerprint"], str) or not fpr.fullmatch(lock["primary_key_fingerprint"]):
    problems.append("primary_key_fingerprint must be 40 uppercase hex")
subkeys = lock["signing_subkey_fingerprints"]
if not isinstance(subkeys, list) or not all(isinstance(f, str) and fpr.fullmatch(f) for f in subkeys) \
        or len(set(subkeys)) != len(subkeys) or lock["primary_key_fingerprint"] in subkeys:
    problems.append("signing_subkey_fingerprints must be distinct 40 uppercase hex subkey fingerprints")
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
     keyring, unusable /dev/kvm, missing host tool, or a keyring that is not
     exactly the pinned primary key with its approved signing subkeys
  2. export the commit (git archive) into a fresh run directory; cargo build
     --release --locked --offline -p providerd -p flowerlyd there
  3. download the pinned image (exact versioned URL) and its signature; verify
     the SHA-256 and, from GnuPG's status output, that an approved signing
     subkey of the pinned primary key signed it; never 'latest'
  4. boot one guest: -accel kvm (no TCG fallback), -nic user,restrict=on with
     only an SSH port forward on 127.0.0.1, qcow2 overlay, unique run id, an
     ephemeral SSH host key generated for this run and pinned (no TOFU)
  5. start a host egress canary on 127.0.0.1 (proven live before and after);
     read QEMU's running command line back from /proc
  6. copy the bundle (export, binaries, manifest, run facts); run
     guest-acceptance.py as root INSIDE the guest
evidence     copy out the guest's evidence; finalize E1-E3 from host facts; bind
             commit, run id, image, hashes and tree state into acceptance.json;
             verify-bound re-derives it; destroy guest, overlay, seed and keys
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
  python3 -I scripts/arch-image-verify.py verify --lock <arch-image.json> --keyring <keyring> --image <image> --signature <image>.sig --out <run>/report/image-verification.json
  qemu-img create -f qcow2 -F qcow2 -b <image> <run>/overlay.qcow2 ${VM_DISK_GB}G
  cloud-localds <run>/seed.img <run>/user-data   (marker + hostname flowerly-kvm-<run id>, no packages)
  qemu-system-x86_64 $(qemu_args "$run_id" "<run>" "<image>" "<port>" | tr '\n' ' ')
  scp <bundle> guest:/opt/flowerly-kvm/ ; ssh guest sudo python3 -I .../guest-acceptance.py /opt/flowerly-kvm
  scp guest:/var/lib/flowerly-kvm/evidence/*.json <run>/report/
  python3 scripts/remote-provider-kvm-evidence.py bind --guest-evidence <evidence> --host-facts <host-facts> --out <run>/report/acceptance.json
  python3 scripts/remote-provider-kvm-evidence.py verify-bound <run>/report/acceptance.json
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
        || die "FLOWERLY_KVM_ARCH_KEYRING must name the keyring holding the pinned primary key"
    [[ -c /dev/kvm && -r /dev/kvm && -w /dev/kvm ]] || die "/dev/kvm is not usable; refusing (no TCG fallback)"
    for tool in "${HOST_TOOLS[@]}"; do
        command -v "$tool" >/dev/null || die "missing host tool: $tool"
    done
    # Structure only, before any download: exactly the pinned primary with
    # its approved signing subkeys, nothing else, no secret material.
    python3 -I "$IMAGE_TOOL" keyring --lock "$TESTBED/arch-image.json" --keyring "$FLOWERLY_KVM_ARCH_KEYRING" >&2 \
        || die "keyring does not hold exactly the pinned primary key and its approved signing subkeys; refusing"
}

lock_field() { python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$TESTBED/arch-image.json" "$1"; }

free_port() { python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])'; }

# Host-side egress canary for E2: a listener on the host's loopback that
# QEMU's slirp gateway (10.0.2.2) maps to when networking is NOT
# restricted. The guest aims at it; with restrict=on it must stay silent.
# The host proves it live with one control connection before and after.
canary_start() {
    local dir=$1
    python3 -I - "$dir" <<'EOF' &
import json, pathlib, socket, sys
out = pathlib.Path(sys.argv[1])
server = socket.socket()
server.bind(("127.0.0.1", 0))
server.listen(16)
(out / "canary.port").write_text(str(server.getsockname()[1]))
count = 0
while True:
    conn, _ = server.accept()
    count += 1
    (out / "canary.count").write_text(str(count))
    conn.close()
EOF
    CANARY_PID=$!
    local _
    for _ in $(seq 1 50); do [[ -s $dir/canary.port ]] && return 0; sleep 0.1; done
    die "egress canary did not start"
}

canary_probe() {
    python3 -I -c 'import socket,sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=5).close()' "$1"
}

execute() {
    local approved=$1
    require_execution_gates "$approved"
    local run_id run cache image port key hostkey ssh_opts guest started
    started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
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
    # Digest and signature from GnuPG's machine-readable status only: the
    # signer must be an approved subkey of the pinned primary.
    python3 -I "$IMAGE_TOOL" verify --lock "$TESTBED/arch-image.json" --keyring "$FLOWERLY_KVM_ARCH_KEYRING" \
        --image "$image" --signature "$run/report/image.sig" --out "$run/report/image-verification.json" \
        || die "image digest or signature does not verify against the pinned key; refusing"
    local image_sha
    image_sha=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha256_actual"])' \
        "$run/report/image-verification.json")
    [[ $image_sha == "$(lock_field sha256)" ]] || die "image SHA-256 mismatch; refusing"

    qemu-img create -q -f qcow2 -F qcow2 -b "$image" "$run/overlay.qcow2" "${VM_DISK_GB}G"
    key="$run/id_ed25519"
    hostkey="$run/guest_host_ed25519"
    ssh-keygen -q -t ed25519 -N "" -f "$key" -C "flowerly-kvm-$run_id"
    # The guest's SSH host key is generated here for this run only and
    # pinned below: no trust-on-first-use, no StrictHostKeyChecking=no.
    ssh-keygen -q -t ed25519 -N "" -f "$hostkey" -C "flowerly-kvm-host-$run_id"
    printf 'flowerly-kvm-%s %s\n' "$run_id" "$(cut -d' ' -f1-2 "$hostkey.pub")" >"$run/known_hosts"
    {
        cat <<EOF
#cloud-config
hostname: flowerly-kvm-${run_id:0:20}
growpart: {mode: auto, devices: ['/']}
resize_rootfs: true
ssh_deletekeys: true
ssh_genkeytypes: []
ssh_keys:
  ed25519_public: '$(cat "$hostkey.pub")'
  ed25519_private: |
EOF
        sed 's/^/    /' "$hostkey"
        cat <<EOF
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
    } >"$run/user-data"
    cloud-localds "$run/seed.img" "$run/user-data"
    port=$(free_port)
    local -a args
    mapfile -t args < <(qemu_args "$run_id" "$run" "$image" "$port")
    printf '%s\n' "${args[@]}" >"$run/report/qemu-args.txt"

    CANARY_PID=""
    cleanup() {
        local pid
        pid=$(cat "$run/qemu.pid" 2>/dev/null || true)
        [[ $pid =~ ^[0-9]+$ ]] && kill "$pid" 2>/dev/null || true
        [[ -n $CANARY_PID ]] && kill "$CANARY_PID" 2>/dev/null || true
        rm -f "$run/overlay.qcow2" "$run/seed.img" "$key" "$key.pub" "$hostkey" "$hostkey.pub" "$run/user-data"
        rm -rf "$run/export" "$run/bundle"
    }
    trap cleanup EXIT
    canary_start "$run"
    local canary_port
    canary_port=$(cat "$run/canary.port")
    local live_before=false live_after=false control=0
    canary_probe "$canary_port" && { live_before=true; control=$((control + 1)); }

    qemu-system-x86_64 "${args[@]}" || die "QEMU/KVM did not start; refusing (no TCG fallback)"
    # What actually runs, read back from the kernel, not what we meant to run.
    local qpid
    qpid=$(cat "$run/qemu.pid")
    tr '\0' '\n' <"/proc/$qpid/cmdline" >"$run/report/qemu-cmdline.txt" || die "cannot read QEMU's command line"

    ssh_opts=(-i "$key" -p "$port" -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$run/known_hosts"
              -o HostKeyAlias="flowerly-kvm-$run_id" -o HostKeyAlgorithms=ssh-ed25519
              -o ConnectTimeout=10 -o BatchMode=yes)
    guest=flowerly-kvm@127.0.0.1
    local _
    for _ in $(seq 1 90); do
        ssh "${ssh_opts[@]}" "$guest" test -f /etc/flowerly-kvm-disposable-guest 2>/dev/null && break
        sleep 5
    done
    # shellcheck disable=SC2029 # $run_id is meant to expand here, on the host.
    ssh "${ssh_opts[@]}" "$guest" "cloud-init status --wait >/dev/null; test \"\$(cat /etc/flowerly-kvm-disposable-guest)\" = $run_id" \
        || die "guest never came up with this run's marker and pinned host key"

    # Bundle: the export, the two binaries, run facts and a manifest.
    local bundle="$run/bundle"
    mkdir -p "$bundle/bin"
    git -C "$ROOT" archive --format=tar --prefix=repo/ "$approved" | tar -x -C "$bundle"
    cp "$run/export/target/release/providerd" "$run/export/target/release/flowerlyd" "$bundle/bin/"
    python3 -I - "$bundle" "$run_id" "$approved" "$(lock_field version)" "$image_sha" "$canary_port" <<'EOF'
import hashlib, json, pathlib, sys
bundle, run_id, commit, version, image_sha, canary_port = pathlib.Path(sys.argv[1]), *sys.argv[2:]
facts = {"run_id": run_id, "git_commit": commit, "git_tree_clean": True,
         "guest_image": f"Arch-Linux-x86_64-cloudimg-{version}", "guest_image_sha256": image_sha,
         "slirp_gateway": "10.0.2.2", "egress_canary_port": int(canary_port)}
(bundle / "run.json").write_text(json.dumps(facts, indent=2))
manifest = {str(p.relative_to(bundle)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(bundle.rglob("*")) if p.is_file() and p.name not in ("run.json", "manifest.json")}
(bundle / "manifest.json").write_text(json.dumps(manifest, indent=2))
EOF
    tar -C "$bundle" -cf - . | ssh "${ssh_opts[@]}" "$guest" "sudo mkdir -p /opt/flowerly-kvm && sudo tar -x -C /opt/flowerly-kvm"
    local guest_status=0
    ssh "${ssh_opts[@]}" "$guest" "sudo python3 -I /opt/flowerly-kvm/repo/testbed/remote-provider-kvm/guest/guest-acceptance.py /opt/flowerly-kvm" \
        >"$run/report/guest.log" 2>&1 || guest_status=$?
    canary_probe "$canary_port" && { live_after=true; control=$((control + 1)); }

    # Evidence phase: copy out, finalize E1-E3 from host facts, bind, verify.
    scp -P "$port" -i "$key" -o StrictHostKeyChecking=yes -o UserKnownHostsFile="$run/known_hosts" \
        -o HostKeyAlias="flowerly-kvm-$run_id" -o HostKeyAlgorithms=ssh-ed25519 -o BatchMode=yes \
        "$guest:/var/lib/flowerly-kvm/evidence/*.json" "$run/report/" || die "no evidence came out of the guest"
    local -a found=("$run/report"/remote-provider-kvm-*.json)
    [[ ${#found[@]} -eq 1 && -f ${found[0]} ]] || die "expected exactly one evidence document from the guest"
    local head_end clean_end=false
    head_end=$(head_sha)
    tree_clean && clean_end=true
    python3 -I - "$run" "$run_id" "$approved" "$head_end" "$clean_end" "$started" \
        "$live_before" "$live_after" "$control" "$TESTBED/arch-image.json" <<'EOF'
import hashlib, json, pathlib, sys, time
(run, run_id, commit, head_end, clean_end, started,
 live_before, live_after, control, lock_path) = sys.argv[1:]
run, lock = pathlib.Path(run), json.loads(pathlib.Path(lock_path).read_text())
sha = lambda path: hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()
count = run / "canary.count"
host = {
    "run_id": run_id, "approved_commit": commit, "head_at_start": commit, "head_at_end": head_end,
    "tree_clean_at_start": True, "tree_clean_at_end": clean_end == "true",
    "started_at_utc": started, "finished_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    # Fingerprints, digests and outcome from arch-image-verify.py; no key material.
    "image": dict(json.loads((run / "report/image-verification.json").read_text()),
                  version=lock["version"], url=lock["url"], arch_image_lock_sha256=sha(lock_path)),
    "qemu": {"cmdline": (run / "report/qemu-cmdline.txt").read_text().splitlines()},
    "egress_canary": {"live_before": live_before == "true", "live_after": live_after == "true",
                      "control_connections": int(control),
                      "total_connections": int(count.read_text()) if count.exists() else 0},
    "build": {"providerd_sha256": sha(run / "bundle/bin/providerd"), "flowerlyd_sha256": sha(run / "bundle/bin/flowerlyd"),
              "unit_sha256": sha(run / "bundle/repo/distro/templates/remote-inference/flowerly-provider-remote@.service"),
              "bundle_manifest_sha256": sha(run / "bundle/manifest.json")},
}
(run / "report/host-facts.json").write_text(json.dumps(host, indent=2, sort_keys=True) + "\n")
EOF
    python3 -I "$EVIDENCE_TOOL" bind --guest-evidence "${found[0]}" --host-facts "$run/report/host-facts.json" \
        --out "$run/report/acceptance.json" >/dev/null
    local verdict=0
    python3 -I "$EVIDENCE_TOOL" verify-bound "$run/report/acceptance.json" | tee "$run/report/verification.json" \
        || verdict=$?
    say "guest exit $guest_status; acceptance record $run/report/acceptance.json; guest destroyed"
    say "only a verify-bound verdict of PASS is an acceptance candidate; the owner decides acceptance"
    return $(( verdict || guest_status ))
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

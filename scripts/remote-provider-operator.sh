#!/usr/bin/env bash
# Operator procedure for one remote inference provider instance
# (ADR-0022, #263). See web-docs/docs/remote-inference-operations.md.
#
#   render    <instance> <https-api-base> <openai|open-webui> <model> <destdir>
#   install   <instance> <https-api-base> <openai|open-webui> <model>   (root)
#   preflight <instance> [evidence-dir]                                 (root)
#   accept    <instance> <approval-reference> [evidence-dir]            (root)
#   disable   <instance>                                                (root)
#   rollback  <instance>                                                (root)
#
# `render` writes every file into <destdir> and needs no privilege; the
# offline tests use it. `install` renders into / and installs the binary,
# but starts and enables nothing. `preflight` re-verifies DNS and runs model
# discovery with the instance restricted by `providerd --discover-only`, so
# no prompt can be sent. `accept` sends the single fixed acceptance prompt
# and must only be run with the owner's explicit approval, whose reference
# is recorded in the evidence. `disable` is the emergency stop. `rollback`
# removes everything `install` wrote.
#
# The API key is never read, printed or copied by this script. The operator
# installs it beforehand by typing it at the systemd-creds prompt:
#   systemd-creds encrypt --name=<instance>.key - /etc/credstore.encrypted/<instance>.key
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATES="$ROOT/distro/templates/remote-inference"
LIBEXEC=/usr/local/libexec/flowerly
BIN="$LIBEXEC/providerd"
CLIENT="$LIBEXEC/remote-provider-acceptance.py"
RESOLV_CONF="${RESOLV_CONF:-/etc/resolv.conf}"
UNIT_TEMPLATE=flowerly-provider-remote@.service

die() { echo "remote-provider-operator: $*" >&2; exit 1; }
usage() { sed -n '4,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,3\}//' >&2; exit 2; }
need_root() { [[ $(id -u) -eq 0 ]] || die "run as root (sudo)"; }

valid_instance() { [[ $1 =~ ^[a-z][a-z0-9-]{0,31}$ ]] || die "invalid instance name: $1"; }
unit_of() { echo "flowerly-provider-remote@$1.service"; }

# Paths relative to a root directory, for one instance.
files_of() {
    local instance=$1
    printf '%s\n' \
        "etc/sysusers.d/flowerly-provider.conf" \
        "etc/systemd/system/$UNIT_TEMPLATE" \
        "etc/systemd/system/$(unit_of "$instance").d/10-operator.conf" \
        "etc/flowerly/providers/$instance.service.toml" \
        "etc/flowerly/providers/$instance.toml"
}

# Loopback nameservers (a local stub such as systemd-resolved) must stay
# reachable through the unit's `IPAddressDeny=localhost`.
loopback_resolvers() {
    [[ -r $RESOLV_CONF ]] || return 0
    awk '$1 == "nameserver" { print $2 }' "$RESOLV_CONF" | while read -r address; do
        case $address in
            127.*|::1) echo "$address" ;;
        esac
    done
}

render() {
    [[ $# -eq 5 ]] || usage
    local instance=$1 api_base=$2 api_style=$3 model=$4 dest=$5
    valid_instance "$instance"
    [[ $api_base =~ ^https://[A-Za-z0-9.-]+(:[0-9]{1,5})?$ ]] || die "api base must be https://host[:port]"
    [[ $api_style == openai || $api_style == open-webui ]] || die "api style must be openai or open-webui"
    [[ $model =~ ^[A-Za-z0-9._:/-]{1,128}$ ]] || die "invalid model id"
    local host=${api_base#https://}
    host=${host%%:*}
    local unit_dir
    unit_dir="$dest/etc/systemd/system/$(unit_of "$instance").d"
    install -d -m 0755 "$dest/etc/sysusers.d" "$dest/etc/systemd/system" "$unit_dir" "$dest/etc/flowerly/providers"

    # The provider account and socket group. Adding `flowerly` to the group
    # belongs to enabling flowerlyd's gates, which this procedure does not do.
    grep -v -e '^m ' -e '^# flowerlyd reaches' "$TEMPLATES/flowerly-provider.sysusers.conf" \
        | install -m 0644 /dev/stdin "$dest/etc/sysusers.d/flowerly-provider.conf"
    install -m 0644 "$TEMPLATES/$UNIT_TEMPLATE" "$dest/etc/systemd/system/$UNIT_TEMPLATE"
    {
        echo "# Written by scripts/remote-provider-operator.sh for instance $instance."
        echo "[Service]"
        echo "ExecStart="
        echo "ExecStart=$BIN --config /etc/flowerly/providers/$instance.service.toml --remote-config /etc/flowerly/providers/$instance.toml"
        local resolver
        for resolver in $(loopback_resolvers); do
            echo "# Local DNS stub; the provider's own address policy still refuses loopback destinations."
            echo "IPAddressAllow=$resolver"
        done
    } | install -m 0644 /dev/stdin "$unit_dir/10-operator.conf"
    printf 'name = "%s"\nruntime_dir = "/run/flowerly-provider"\nusers_group = "flowerly-provider-clients"\n' \
        "$instance" | install -m 0644 /dev/stdin "$dest/etc/flowerly/providers/$instance.service.toml"
    {
        echo "# Destination $host. No key here: the key is the systemd credential $instance.key."
        echo "[remote]"
        echo "api_base = \"$api_base\""
        echo "api_style = \"$api_style\""
        echo "models = [\"$model\"]"
        echo "credential = \"$instance.key\""
        echo "max_tokens_cap = 1024"
        echo "request_timeout_ms = 120000"
        echo "max_concurrency = 1"
    } | install -m 0644 /dev/stdin "$dest/etc/flowerly/providers/$instance.toml"
}

install_instance() {
    need_root
    [[ $# -eq 4 ]] || usage
    local instance=$1
    valid_instance "$instance"
    [[ -f /etc/credstore.encrypted/$instance.key ]] \
        || die "missing /etc/credstore.encrypted/$instance.key; install the key with systemd-creds first"
    [[ -x $ROOT/target/release/providerd ]] || die "build first: cargo build --release -p providerd"
    local path
    while read -r path; do
        [[ $path == "etc/systemd/system/$UNIT_TEMPLATE" && -e /$path ]] && cmp -s "/$path" "$TEMPLATES/$UNIT_TEMPLATE" && continue
        [[ $path == etc/sysusers.d/* && -e /$path ]] && continue
        [[ ! -e /$path ]] || die "refusing to overwrite /$path; run rollback first"
    done < <(files_of "$instance")
    render "$@" /
    install -d -m 0755 "$LIBEXEC"
    install -m 0755 "$ROOT/target/release/providerd" "$BIN"
    install -m 0755 "$ROOT/scripts/remote-provider-acceptance.py" "$CLIENT"
    systemd-sysusers /etc/sysusers.d/flowerly-provider.conf
    systemctl daemon-reload
    echo "installed $(unit_of "$instance"): not started, not enabled; nothing was sent"
}

# --- evidence ------------------------------------------------------------

evidence_begin() {
    local dir=$1 step=$2 instance=$3
    install -d -m 0750 "$dir"
    EVIDENCE="$dir/$(date -u +%Y%m%dT%H%M%SZ)-$instance-$step.json"
    SINCE=$(date -u '+%Y-%m-%d %H:%M:%S UTC')
    STEP_REPORTS=()
}

sha_of() { [[ -e $1 ]] && sha256sum "$1" | cut -d' ' -f1 || echo "absent"; }

evidence_write() {
    local instance=$1 step=$2 approval=$3 unit journal reports status=0
    unit=$(unit_of "$instance")
    journal=$(mktemp)
    reports=$(mktemp)
    journalctl --no-pager -o cat -u "$unit" --since "$SINCE" >"$journal" 2>/dev/null || true
    printf '%s\n' "${STEP_REPORTS[@]}" >"$reports"
    EVIDENCE_PATH="$EVIDENCE" STEP="$step" INSTANCE="$instance" APPROVAL="$approval" \
        JOURNAL="$journal" REPORTS="$reports" \
        GIT_COMMIT="$(git -c safe.directory="$ROOT" -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)" \
        GIT_CLEAN="$(git -c safe.directory="$ROOT" -C "$ROOT" diff --quiet HEAD 2>/dev/null && echo yes || echo no)" \
        SYSTEMD_VERSION="$(systemctl --version | head -1)" \
        EXPOSURE="$(systemd-analyze security --no-pager "$unit" 2>/dev/null | tail -1 || true)" \
        SHA_PROVIDERD="$(sha_of "$BIN")" SHA_CLIENT="$(sha_of "$CLIENT")" \
        SHA_UNIT="$(sha_of "/etc/systemd/system/$UNIT_TEMPLATE")" \
        SHA_DROPIN="$(sha_of "/etc/systemd/system/$unit.d/10-operator.conf")" \
        SHA_CONFIG="$(sha_of "/etc/flowerly/providers/$instance.toml")" \
        python3 -I "$ROOT/scripts/remote-provider-acceptance.py" evidence || status=$?
    rm -f "$journal" "$reports"
    return $status
}

# Run one client step as the provider account; keep its JSON report.
client_step() {
    local report status=0
    report=$(runuser -u flowerly-provider -- python3 -I "$CLIENT" "$@") || status=$?
    printf '%s\n' "$report"
    STEP_REPORTS+=("$report")
    return $status
}

dns_step() {
    local instance=$1 report status=0 host
    host=$(sed -n 's|^api_base = "https://\([^":/]*\).*|\1|p' "/etc/flowerly/providers/$instance.toml")
    [[ -n $host ]] || die "no api_base in /etc/flowerly/providers/$instance.toml"
    report=$(python3 -I "$CLIENT" dns "$host") || status=$?
    printf '%s\n' "$report"
    STEP_REPORTS+=("$report")
    return $status
}

model_of() {
    sed -n 's|^models = \["\([^"]*\)"\]|\1|p' "/etc/flowerly/providers/$1.toml"
}

wait_socket() {
    local socket=$1 _
    for _ in $(seq 1 100); do
        [[ -S $socket ]] && return 0
        sleep 0.1
    done
    return 1
}

preflight() {
    need_root
    [[ $# -ge 1 && $# -le 2 ]] || usage
    local instance=$1 dir=${2:-/var/log/flowerly-provider-acceptance} unit runtime status=0
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    runtime="/run/systemd/system/$unit.d"
    evidence_begin "$dir" preflight "$instance"
    dns_step "$instance" || status=$?
    if [[ $status -eq 0 ]]; then
        # Runtime-only drop-in: gone on reboot, removed below either way.
        install -d -m 0755 "$runtime"
        printf '[Service]\nExecStart=\nExecStart=%s --config /etc/flowerly/providers/%s.service.toml --remote-config /etc/flowerly/providers/%s.toml --discover-only\n' \
            "$BIN" "$instance" "$instance" | install -m 0644 /dev/stdin "$runtime/50-discover-only.conf"
        systemctl daemon-reload
        if systemctl start "$unit" && wait_socket "/run/flowerly-provider/$instance.sock"; then
            client_step discover "/run/flowerly-provider/$instance.sock" "$(model_of "$instance")" || status=$?
        else
            echo "the unit did not start; see: journalctl -u $unit" >&2
            status=1
        fi
        systemctl stop "$unit" || true
        rm -f "$runtime/50-discover-only.conf"
        rmdir "$runtime" 2>/dev/null || true
        systemctl daemon-reload
    fi
    evidence_write "$instance" preflight "" || status=1
    return $status
}

accept() {
    need_root
    [[ $# -ge 2 && $# -le 3 ]] || usage
    local instance=$1 approval=$2 dir=${3:-/var/log/flowerly-provider-acceptance} unit status=0
    valid_instance "$instance"
    [[ -n $approval ]] || die "an owner approval reference is required"
    unit=$(unit_of "$instance")
    [[ ! -e /run/systemd/system/$unit.d/50-discover-only.conf ]] || die "a discovery-only drop-in is still present"
    evidence_begin "$dir" accept "$instance"
    dns_step "$instance" || status=$?
    if [[ $status -eq 0 ]]; then
        if systemctl start "$unit" && wait_socket "/run/flowerly-provider/$instance.sock"; then
            client_step accept "/run/flowerly-provider/$instance.sock" "$(model_of "$instance")" || status=$?
        else
            echo "the unit did not start; see: journalctl -u $unit" >&2
            status=1
        fi
        # One request only: the instance does not keep running.
        systemctl stop "$unit" || true
    fi
    evidence_write "$instance" accept "$approval" || status=1
    return $status
}

disable_instance() {
    need_root
    [[ $# -eq 1 ]] || usage
    local instance=$1 unit
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    # Emergency stop: the instance cannot be started again until unmasked.
    systemctl mask --now "$unit"
    # flowerlyd's gate, if an administrator enabled it: absent = disabled.
    if [[ -e /etc/flowerly/remote-providers.toml ]]; then
        mv /etc/flowerly/remote-providers.toml "/etc/flowerly/remote-providers.toml.disabled-$(date -u +%Y%m%dT%H%M%SZ)"
        systemctl try-restart flowerlyd.service || true
    fi
    echo "disabled: $unit is stopped and masked; flowerlyd's remote gate is absent"
}

rollback() {
    need_root
    [[ $# -eq 1 ]] || usage
    local instance=$1 unit path
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    systemctl stop "$unit" 2>/dev/null || true
    systemctl unmask "$unit" 2>/dev/null || true
    rm -rf "/run/systemd/system/$unit.d"
    while read -r path; do
        case $path in
            # Shared by every instance: removed with the last one.
            "etc/systemd/system/$UNIT_TEMPLATE"|etc/sysusers.d/*) ;;
            *) rm -f "/$path" ;;
        esac
    done < <(files_of "$instance")
    rmdir "/etc/systemd/system/$unit.d" 2>/dev/null || true
    if ! compgen -G "/etc/flowerly/providers/*.toml" >/dev/null; then
        rm -f "/etc/systemd/system/$UNIT_TEMPLATE" /etc/sysusers.d/flowerly-provider.conf "$BIN" "$CLIENT"
        rmdir "$LIBEXEC" /etc/flowerly/providers /etc/flowerly 2>/dev/null || true
        rm -rf /run/flowerly-provider
        if getent passwd flowerly-provider >/dev/null; then userdel flowerly-provider; fi
        for group in flowerly-provider flowerly-provider-clients; do
            if getent group "$group" >/dev/null; then groupdel "$group"; fi
        done
    fi
    systemctl daemon-reload
    echo "rolled back $unit; /etc/credstore.encrypted/$instance.key was left for the operator to remove"
}

[[ $# -ge 1 ]] || usage
command=$1
shift
case $command in
    render) render "$@" ;;
    install) install_instance "$@" ;;
    preflight) preflight "$@" ;;
    accept) accept "$@" ;;
    disable) disable_instance "$@" ;;
    rollback) rollback "$@" ;;
    *) usage ;;
esac

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
#   rollback-shared                                                     (root)
#
# `render` writes the instance's files into <destdir> without privilege.
# `install` renders into the system, installs the binary and records what
# it created in a root-only ledger; it starts and enables nothing.
# `preflight` re-verifies DNS and runs model discovery with the instance
# restricted by `providerd --discover-only`, so no prompt can be sent.
# `accept` sends the single fixed acceptance prompt. Its approval reference
# is an operator attestation recorded in the evidence, not proof of the
# owner's authorization: nothing here verifies it. `disable` is the
# emergency stop. `rollback` removes only what the ledger says this tool
# created, only while unmodified, and keeps shared resources while any
# other instance, installation, process or group member still uses them;
# `rollback-shared` retries that shared cleanup once they no longer do.
#
# The API key is never read, printed or copied by this script. The operator
# installs it beforehand by typing it at the systemd-creds prompt:
#   systemd-creds encrypt --name=<instance>.key - /etc/credstore.encrypted/<instance>.key
#
# Offline tests run install and rollback against a scratch system root:
# FLOWERLY_OPERATOR_SYSROOT=<dir> with FLOWERLY_OPERATOR_TEST=1, as a
# non-root user. Every system command is then logged, not executed, and
# accounts, groups, processes and active units come from files in that root.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMPLATES="$ROOT/distro/templates/remote-inference"
RESOLV_CONF="${RESOLV_CONF:-/etc/resolv.conf}"
UNIT_TEMPLATE=flowerly-provider-remote@.service
PROVIDER_USER=flowerly-provider
CLIENT_GROUP=flowerly-provider-clients
# The only account that may share the socket group: flowerlyd's, once an
# administrator enables its gates.
GATE_USER=flowerly

# System paths, without the leading slash; `at` places them in the root.
LIBEXEC=usr/local/libexec/flowerly
BIN=$LIBEXEC/providerd
CLIENT=$LIBEXEC/remote-provider-acceptance.py
SYSUSERS=etc/sysusers.d/flowerly-provider.conf
UNIT=etc/systemd/system/$UNIT_TEMPLATE
LEDGER=var/lib/flowerly-provider-operator
RUNTIME=run/flowerly-provider

die() { echo "remote-provider-operator: $*" >&2; exit 1; }
warn() { echo "remote-provider-operator: $*" >&2; }
usage() { sed -n '4,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,3\}//' >&2; exit 2; }

SYSROOT=${FLOWERLY_OPERATOR_SYSROOT:-/}
TEST_MODE=0
if [[ $SYSROOT != / ]]; then
    [[ ${FLOWERLY_OPERATOR_TEST:-} == 1 ]] || die "FLOWERLY_OPERATOR_SYSROOT is for the offline tests only"
    [[ $(id -u) -ne 0 ]] || die "test mode refuses to run as root"
    [[ $SYSROOT == /* && -d $SYSROOT ]] || die "FLOWERLY_OPERATOR_SYSROOT must be an existing absolute directory"
    TEST_MODE=1
fi
at() { printf '%s/%s' "${SYSROOT%/}" "$1"; }

need_root() {
    (( TEST_MODE )) && return 0
    [[ $(id -u) -eq 0 ]] || die "run as root (sudo)"
}
no_test_mode() { (( ! TEST_MODE )) || die "$1 is not available in test mode"; }

valid_instance() { [[ $1 =~ ^[a-z][a-z0-9-]{0,31}$ ]] || die "invalid instance name: $1"; }
unit_of() { echo "flowerly-provider-remote@$1.service"; }
dropin_of() { echo "etc/systemd/system/$(unit_of "$1").d/10-operator.conf"; }
instance_files() {
    printf '%s\n' "$(dropin_of "$1")" "etc/flowerly/providers/$1.service.toml" "etc/flowerly/providers/$1.toml"
}
sha() { sha256sum "$1" | cut -c1-64; }

# --- system access (logged, not executed, in test mode) -----------------

system() {
    if (( TEST_MODE )); then
        echo "$*" >>"$(at test/commands.log)"
        case $1 in
            systemd-sysusers) fake_sysusers ;;
            userdel) sed -i "/^$2:/d" "$(at etc/passwd)" ;;
            groupdel) sed -i "/^$2:/d" "$(at etc/group)" ;;
        esac
        return 0
    fi
    "$@"
}

fake_sysusers() {
    touch "$(at etc/passwd)" "$(at etc/group)"
    grep -q "^$CLIENT_GROUP:" "$(at etc/group)" || echo "$CLIENT_GROUP:x:989:" >>"$(at etc/group)"
    grep -q "^$PROVIDER_USER:" "$(at etc/group)" || echo "$PROVIDER_USER:x:990:" >>"$(at etc/group)"
    grep -q "^$PROVIDER_USER:" "$(at etc/passwd)" \
        || echo "$PROVIDER_USER:x:990:990:Flowerly OS remote inference provider:/:/usr/sbin/nologin" >>"$(at etc/passwd)"
}

passwd_entry() {
    if (( TEST_MODE )); then grep "^$1:" "$(at etc/passwd)" 2>/dev/null || true
    else getent passwd "$1" || true; fi
}
group_entry() {
    if (( TEST_MODE )); then grep "^$1:" "$(at etc/group)" 2>/dev/null || true
    else getent group "$1" || true; fi
}
all_groups() {
    if (( TEST_MODE )); then cat "$(at etc/group)" 2>/dev/null || true
    else getent group || true; fi
}
# Explicit members of a group, one per line.
group_members() { group_entry "$1" | cut -d: -f4 | tr ',' '\n' | sed '/^$/d'; }
# Groups that list a user as an explicit (supplementary) member.
groups_of_member() { all_groups | awk -F: -v user="$1" '{ n = split($4, m, ","); for (i = 1; i <= n; i++) if (m[i] == user) print $1 }'; }
user_has_processes() {
    if (( TEST_MODE )); then grep -qx "$1" "$(at test/processes)" 2>/dev/null
    else pgrep -u "$1" >/dev/null 2>&1; fi
}
active_provider_units() {
    if (( TEST_MODE )); then cat "$(at test/active-units)" 2>/dev/null || true
    else systemctl list-units --plain --no-legend --state=active,activating,reloading 'flowerly-provider-remote@*' 2>/dev/null | awk '{ print $1 }'; fi
}

# --- render --------------------------------------------------------------

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

exec_line() {
    local instance=$1 extra=${2:-}
    echo "ExecStart=/$BIN --config /etc/flowerly/providers/$instance.service.toml --remote-config /etc/flowerly/providers/$instance.toml --require-isolated-identity$extra"
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
        | install -m 0644 /dev/stdin "$dest/$SYSUSERS"
    install -m 0644 "$TEMPLATES/$UNIT_TEMPLATE" "$dest/$UNIT"
    {
        echo "# Written by scripts/remote-provider-operator.sh for instance $instance."
        echo "[Service]"
        echo "ExecStart="
        exec_line "$instance"
        local resolver
        for resolver in $(loopback_resolvers); do
            echo "# Local DNS stub; the provider's own address policy still refuses loopback destinations."
            echo "IPAddressAllow=$resolver"
        done
    } | install -m 0644 /dev/stdin "$unit_dir/10-operator.conf"
    printf 'name = "%s"\nruntime_dir = "/run/flowerly-provider"\nusers_group = "%s"\n' \
        "$instance" "$CLIENT_GROUP" | install -m 0644 /dev/stdin "$dest/etc/flowerly/providers/$instance.service.toml"
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

# --- identity checks -----------------------------------------------------

# The socket group admits whoever is in it to the remote key, without
# consent: only the provider and flowerlyd may ever be members. The
# provider account must hold no other group, or it gains that group's
# access (systemd applies the account's supplementary groups).
check_identities() {
    local member group entry
    while read -r member; do
        [[ -z $member || $member == "$PROVIDER_USER" || $member == "$GATE_USER" ]] \
            || die "group $CLIENT_GROUP has member $member; only $GATE_USER may join it"
    done < <(group_members "$CLIENT_GROUP")
    entry=$(passwd_entry "$PROVIDER_USER")
    if [[ -n $entry ]]; then
        [[ $(cut -d: -f3 <<<"$entry") != 0 ]] || die "$PROVIDER_USER has uid 0"
        while read -r group; do
            [[ -z $group || $group == "$CLIENT_GROUP" ]] \
                || die "$PROVIDER_USER is a member of $group; the provider must hold no other group"
        done < <(groups_of_member "$PROVIDER_USER")
    fi
}

# --- ledger --------------------------------------------------------------

ledger_instance() { at "$LEDGER/instances/$1"; }
ledger_shared() { at "$LEDGER/shared"; }
shared_owned() { [[ -f $(ledger_shared) ]] && awk -F'\t' -v k="$1" -v n="$2" '$1 == k && $2 == n { found = 1 } END { exit !found }' "$(ledger_shared)"; }
shared_sha() { awk -F'\t' -v n="$1" '$1 == "file" && $2 == n { print $3 }' "$(ledger_shared)" 2>/dev/null; }
ledger_add_shared() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:--}" >>"$(ledger_shared)"; }

# Decide, without changing anything, how a shared file is provided.
plan_shared_file() {
    local rel=$1 source=$2 target
    target=$(at "$rel")
    if [[ ! -e $target && ! -L $target ]]; then
        echo create
    elif [[ -L $target || ! -f $target ]]; then
        die "/$rel is not a regular file; it belongs to another installation"
    elif cmp -s "$target" "$source"; then
        echo reuse
    elif shared_owned file "$rel"; then
        die "/$rel differs from this build; roll back every instance before installing a different build"
    else
        die "/$rel exists and was not installed by this tool; refusing to replace it"
    fi
}

install_instance() {
    need_root
    [[ $# -eq 4 ]] || usage
    local instance=$1 api_base=$2 api_style=$3 model=$4
    valid_instance "$instance"
    local source_bin=${FLOWERLY_OPERATOR_PROVIDERD:-$ROOT/target/release/providerd}
    [[ -x $source_bin ]] || die "build first: cargo build --release -p providerd"
    [[ -f $(at "etc/credstore.encrypted/$instance.key") ]] \
        || die "missing /etc/credstore.encrypted/$instance.key; install the key with systemd-creds first"
    [[ ! -e $(ledger_instance "$instance") ]] || die "instance $instance is already installed"
    local rel
    while read -r rel; do
        [[ ! -e $(at "$rel") ]] || die "refusing to overwrite /$rel; it was not installed by this tool for $instance"
    done < <(instance_files "$instance")
    check_identities

    staging=$(mktemp -d)
    trap 'rm -rf "${staging:-}"' EXIT
    render "$instance" "$api_base" "$api_style" "$model" "$staging"
    cp "$source_bin" "$staging/providerd"
    cp "$ROOT/scripts/remote-provider-acceptance.py" "$staging/client"
    # Every decision is made before the first change.
    declare -A plan source
    source[$UNIT]=$staging/$UNIT
    source[$SYSUSERS]=$staging/$SYSUSERS
    source[$BIN]=$staging/providerd
    source[$CLIENT]=$staging/client
    for rel in "$UNIT" "$SYSUSERS" "$BIN" "$CLIENT"; do
        plan[$rel]=$(plan_shared_file "$rel" "${source[$rel]}")
    done

    install -d -m 0700 "$(at "$LEDGER")" "$(at "$LEDGER/instances")"
    for rel in "$UNIT" "$SYSUSERS" "$BIN" "$CLIENT"; do
        [[ ${plan[$rel]} == create ]] || continue
        local mode=0644
        [[ $rel == "$BIN" || $rel == "$CLIENT" ]] && mode=0755
        install -D -m "$mode" "${source[$rel]}" "$(at "$rel")"
        ledger_add_shared file "$rel" "$(sha "$(at "$rel")")"
    done
    local had_user had_group had_clients
    had_user=$(passwd_entry "$PROVIDER_USER")
    had_group=$(group_entry "$PROVIDER_USER")
    had_clients=$(group_entry "$CLIENT_GROUP")
    system systemd-sysusers "/$SYSUSERS"
    [[ -n $had_user || -z $(passwd_entry "$PROVIDER_USER") ]] || ledger_add_shared user "$PROVIDER_USER"
    [[ -n $had_group || -z $(group_entry "$PROVIDER_USER") ]] || ledger_add_shared group "$PROVIDER_USER"
    [[ -n $had_clients || -z $(group_entry "$CLIENT_GROUP") ]] || ledger_add_shared group "$CLIENT_GROUP"
    check_identities

    : >"$(ledger_instance "$instance").tmp"
    while read -r rel; do
        install -D -m 0644 "$staging/$rel" "$(at "$rel")"
        printf 'file\t%s\t%s\n' "$rel" "$(sha "$(at "$rel")")" >>"$(ledger_instance "$instance").tmp"
    done < <(instance_files "$instance")
    mv "$(ledger_instance "$instance").tmp" "$(ledger_instance "$instance")"
    system systemctl daemon-reload
    echo "installed $(unit_of "$instance"): not started, not enabled; nothing was sent"
}

# --- rollback ------------------------------------------------------------

# Remove a file only if it is still the one this tool wrote.
remove_if_unmodified() {
    local rel=$1 expected=$2 target
    target=$(at "$rel")
    if [[ ! -e $target && ! -L $target ]]; then
        return 0
    elif [[ -L $target || ! -f $target ]]; then
        warn "kept /$rel: it is no longer the regular file this tool installed"
        return 1
    elif [[ $(sha "$target") != "$expected" ]]; then
        warn "kept /$rel: modified since install"
        return 1
    fi
    rm -f "$target"
}

validate_shared_ledger() {
    local kind rel expected
    while IFS=$'\t' read -r kind rel expected; do
        case "$kind:$rel" in
            "file:$UNIT"|"file:$SYSUSERS"|"file:$BIN"|"file:$CLIENT") [[ $expected =~ ^[0-9a-f]{64}$ ]] ;;
            "user:$PROVIDER_USER"|"group:$PROVIDER_USER"|"group:$CLIENT_GROUP") [[ $expected == - ]] ;;
            *) false ;;
        esac || die "shared ledger names an unexpected entry ($kind $rel); nothing removed"
    done <"$(ledger_shared)"
}

other_users() {
    local instance=$1 path name unit
    for path in "$(at "$LEDGER/instances")"/*; do
        [[ -e $path ]] || continue
        name=${path##*/}
        [[ $name == "$instance" ]] || echo "instance $name (this tool)"
    done
    for path in "$(at etc/flowerly/providers)"/*.toml; do
        [[ -e $path ]] || continue
        name=${path##*/}
        name=${name%.service.toml}
        name=${name%.toml}
        [[ $name == "$instance" || -e $(ledger_instance "$name") ]] || echo "provider config $name (another installation)"
    done
    while read -r unit; do
        [[ -z $unit || $unit == "$(unit_of "$instance")" ]] || echo "active unit $unit"
    done < <(active_provider_units)
}

rollback() {
    need_root
    [[ $# -eq 1 ]] || usage
    local instance=$1 unit status=0 kind rel expected
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    local ledger
    ledger=$(ledger_instance "$instance")
    [[ -f $ledger ]] || die "no install ledger for $instance: this tool did not install it, so nothing is removed"
    # Validate the whole ledger before touching anything.
    local -A allowed=()
    while read -r rel; do allowed[$rel]=1; done < <(instance_files "$instance")
    while IFS=$'\t' read -r kind rel expected; do
        [[ $kind == file && -n ${allowed[$rel]:-} && $expected =~ ^[0-9a-f]{64}$ ]] \
            || die "ledger for $instance names an unexpected entry ($kind $rel); nothing removed"
    done <"$ledger"
    [[ ! -f $(ledger_shared) ]] || validate_shared_ledger

    system systemctl stop "$unit" || true
    system systemctl unmask "$unit" || true
    rm -f "$(at "run/systemd/system/$unit.d/50-discover-only.conf")"
    rmdir "$(at "run/systemd/system/$unit.d")" 2>/dev/null || true
    while IFS=$'\t' read -r kind rel expected; do
        remove_if_unmodified "$rel" "$expected" || status=1
    done <"$ledger"
    rmdir "$(at "etc/systemd/system/$unit.d")" 2>/dev/null || true
    # This instance's runtime files only, never the shared directory.
    for rel in "$RUNTIME/$instance.sock" "$RUNTIME/$instance.sock.lock"; do
        if [[ -L $(at "$rel") ]]; then warn "kept /$rel: a symlink"; status=1
        elif [[ -e $(at "$rel") ]]; then rm -f "$(at "$rel")"; fi
    done
    rm -f "$ledger"

    local users line
    users=$(other_users "$instance")
    if [[ -n $users ]]; then
        echo "kept the shared unit template, binary, client, account and groups; still used by:"
        while read -r line; do echo "  $line"; done <<<"$users"
    elif [[ -f $(ledger_shared) ]]; then
        rollback_shared || status=1
    fi
    rmdir "$(at "$RUNTIME")" "$(at etc/flowerly/providers)" "$(at etc/flowerly)" \
        "$(at "$LEDGER/instances")" "$(at "$LEDGER")" 2>/dev/null || true
    system systemctl daemon-reload
    echo "rolled back $unit; /etc/credstore.encrypted/$instance.key was left for the operator to remove"
    return $status
}

# Shared resources kept by an earlier rollback, once nothing uses them.
rollback_shared_command() {
    need_root
    [[ $# -eq 0 ]] || usage
    [[ -f $(ledger_shared) ]] || die "no shared resources recorded by this tool"
    validate_shared_ledger
    local users line
    users=$(other_users "")
    if [[ -n $users ]]; then
        echo "kept the shared resources; still used by:"
        while read -r line; do echo "  $line"; done <<<"$users"
        return 1
    fi
    local status=0
    rollback_shared || status=1
    rmdir "$(at "$RUNTIME")" "$(at etc/flowerly/providers)" "$(at etc/flowerly)" \
        "$(at "$LEDGER/instances")" "$(at "$LEDGER")" 2>/dev/null || true
    system systemctl daemon-reload
    return $status
}

rollback_shared() {
    local status=0 kind name expected kept member
    kept=$(mktemp)
    while IFS=$'\t' read -r kind name expected; do
        [[ $kind == file ]] || continue
        remove_if_unmodified "$name" "$expected" || { printf '%s\t%s\t%s\n' "$kind" "$name" "$expected" >>"$kept"; status=1; }
    done <"$(ledger_shared)"
    if shared_owned user "$PROVIDER_USER" && [[ -n $(passwd_entry "$PROVIDER_USER") ]]; then
        if user_has_processes "$PROVIDER_USER"; then
            warn "kept account $PROVIDER_USER: it still runs processes"
            printf 'user\t%s\t-\n' "$PROVIDER_USER" >>"$kept"
            status=1
        else
            system userdel "$PROVIDER_USER"
        fi
    fi
    for name in "$PROVIDER_USER" "$CLIENT_GROUP"; do
        shared_owned group "$name" && [[ -n $(group_entry "$name") ]] || continue
        member=$(group_members "$name" | grep -vx "$PROVIDER_USER" | head -1 || true)
        # A kept account keeps its groups: its processes may hold them.
        if [[ -n $member || -n $(passwd_entry "$PROVIDER_USER") ]]; then
            warn "kept group $name: still in use${member:+ by $member}"
            printf 'group\t%s\t-\n' "$name" >>"$kept"
            status=1
        else
            system groupdel "$name"
        fi
    done
    rmdir "$(at "$LIBEXEC")" 2>/dev/null || true
    if [[ -s $kept ]]; then mv "$kept" "$(ledger_shared)"; else rm -f "$kept" "$(ledger_shared)"; fi
    return $status
}

# --- evidence ------------------------------------------------------------

evidence_begin() {
    local dir=$1 step=$2 instance=$3
    install -d -m 0750 "$dir"
    EVIDENCE="$dir/$(date -u +%Y%m%dT%H%M%SZ)-$instance-$step.json"
    SINCE=$(date -u '+%Y-%m-%d %H:%M:%S UTC')
    STEP_REPORTS=()
}

sha_or_absent() { if [[ -e $1 ]]; then sha "$1"; else echo absent; fi; }

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
        SHA_PROVIDERD="$(sha_or_absent "/$BIN")" SHA_CLIENT="$(sha_or_absent "/$CLIENT")" \
        SHA_UNIT="$(sha_or_absent "/$UNIT")" \
        SHA_DROPIN="$(sha_or_absent "/$(dropin_of "$instance")")" \
        SHA_CONFIG="$(sha_or_absent "/etc/flowerly/providers/$instance.toml")" \
        CLIENT_GROUP_MEMBERS="$(group_members "$CLIENT_GROUP" | paste -sd, -)" \
        PROVIDER_GROUPS="$(groups_of_member "$PROVIDER_USER" | paste -sd, -)" \
        python3 -I "$ROOT/scripts/remote-provider-acceptance.py" evidence || status=$?
    rm -f "$journal" "$reports"
    return $status
}

# Run one client step as the provider account; keep its JSON report.
client_step() {
    local report status=0
    report=$(runuser -u "$PROVIDER_USER" -- python3 -I "/$CLIENT" "$@") || status=$?
    printf '%s\n' "$report"
    STEP_REPORTS+=("$report")
    return $status
}

dns_step() {
    local instance=$1 report status=0 host
    host=$(sed -n 's|^api_base = "https://\([^":/]*\).*|\1|p' "/etc/flowerly/providers/$instance.toml")
    [[ -n $host ]] || die "no api_base in /etc/flowerly/providers/$instance.toml"
    report=$(python3 -I "/$CLIENT" dns "$host") || status=$?
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

installed_here() {
    [[ -f /$LEDGER/instances/$1 ]] || die "instance $1 was not installed by this tool"
    check_identities
}

preflight() {
    need_root
    no_test_mode preflight
    [[ $# -ge 1 && $# -le 2 ]] || usage
    local instance=$1 dir=${2:-/var/log/flowerly-provider-acceptance} unit runtime status=0
    valid_instance "$instance"
    installed_here "$instance"
    unit=$(unit_of "$instance")
    runtime="/run/systemd/system/$unit.d"
    evidence_begin "$dir" preflight "$instance"
    dns_step "$instance" || status=$?
    if [[ $status -eq 0 ]]; then
        # Runtime-only drop-in: gone on reboot, removed below either way.
        install -d -m 0755 "$runtime"
        printf '[Service]\nExecStart=\n%s\n' "$(exec_line "$instance" " --discover-only")" \
            | install -m 0644 /dev/stdin "$runtime/50-discover-only.conf"
        systemctl daemon-reload
        if systemctl start "$unit" && wait_socket "/$RUNTIME/$instance.sock"; then
            client_step discover "/$RUNTIME/$instance.sock" "$(model_of "$instance")" || status=$?
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
    no_test_mode accept
    [[ $# -ge 2 && $# -le 3 ]] || usage
    local instance=$1 approval=$2 dir=${3:-/var/log/flowerly-provider-acceptance} unit status=0
    valid_instance "$instance"
    installed_here "$instance"
    [[ -n $approval ]] || die "an approval reference is required (recorded as the operator's attestation)"
    unit=$(unit_of "$instance")
    [[ ! -e /run/systemd/system/$unit.d/50-discover-only.conf ]] || die "a discovery-only drop-in is still present"
    evidence_begin "$dir" accept "$instance"
    dns_step "$instance" || status=$?
    if [[ $status -eq 0 ]]; then
        if systemctl start "$unit" && wait_socket "/$RUNTIME/$instance.sock"; then
            client_step accept "/$RUNTIME/$instance.sock" "$(model_of "$instance")" || status=$?
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
    no_test_mode disable
    [[ $# -eq 1 ]] || usage
    local instance=$1 unit
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    # Emergency stop for any instance, installed by this tool or not: it
    # cannot be started again until unmasked.
    systemctl mask --now "$unit"
    # flowerlyd's gate, if an administrator enabled it: absent = disabled.
    # This disables remote inference for every provider, deliberately.
    if [[ -e /etc/flowerly/remote-providers.toml ]]; then
        mv /etc/flowerly/remote-providers.toml "/etc/flowerly/remote-providers.toml.disabled-$(date -u +%Y%m%dT%H%M%SZ)"
        systemctl try-restart flowerlyd.service || true
    fi
    echo "disabled: $unit is stopped and masked; flowerlyd's remote gate is absent"
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
    rollback-shared) rollback_shared_command "$@" ;;
    *) usage ;;
esac

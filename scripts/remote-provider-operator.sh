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
# created, only while unmodified, and keeps every file it could not remove
# recorded so a rerun continues where it stopped. Shared resources are
# touched only after every usage check passed: while any other instance,
# installation, process, group member or an inconclusive check remains,
# nothing shared changes. `rollback-shared` retries that cleanup later.
#
# Every subcommand but `render` holds one exclusive lock,
# /run/flowerly-provider-operator.lock, for its whole run; a second one
# refuses instead of waiting. `disable` masks the unit first, without the
# lock, so an emergency stop is never held up, then waits for the lock.
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
umask 022

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
LOCK=run/flowerly-provider-operator.lock

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
# Who must own every directory on a path this tool changes.
OWNER_UID=0
(( ! TEST_MODE )) || OWNER_UID=$(id -u)

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
# The usage checks below fail, rather than report "nothing", when they
# cannot tell; callers treat that as "still in use".
provider_has_processes() {
    if (( TEST_MODE )); then
        [[ ! -e $(at test/processes-error) ]] || return 2
        grep -qx "$1" "$(at test/processes)" 2>/dev/null && return 0
        return 1
    fi
    [[ -n $(passwd_entry "$1") ]] || return 1
    local status=0
    pgrep -u "$1" >/dev/null 2>&1 || status=$?
    return "$status"
}
active_provider_units() {
    if (( TEST_MODE )); then
        [[ ! -e $(at test/units-error) ]] || return 1
        cat "$(at test/active-units)" 2>/dev/null || true
        return 0
    fi
    local units
    units=$(systemctl list-units --plain --no-legend --state=active,activating,reloading 'flowerly-provider-remote@*') || return 1
    awk '{ print $1 }' <<<"$units"
}
# Pids of processes executing the shared binary, whoever runs them.
binary_users() {
    if (( TEST_MODE )); then
        [[ ! -e $(at test/exe-error) ]] || return 1
        awk -v bin="/$BIN" 'index($2, bin) == 1 { print $1 }' "$(at test/exe)" 2>/dev/null | paste -sd' ' -
        return 0
    fi
    [[ -r /proc/1/exe || -L /proc/1/exe ]] || return 1
    local exe pids=()
    for exe in /proc/[0-9]*/exe; do
        [[ $(readlink "$exe" 2>/dev/null) == "/$BIN"* ]] && pids+=("$(basename "$(dirname "$exe")")")
    done
    echo "${pids[*]}"
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

# --- path safety and the operator lock ----------------------------------

# Every directory from the root down to the parent of $1 must be a real
# directory that only root can change. Nobody else can then redirect the
# path between a check and the change made through it, and a symlinked
# directory never leads a removal outside the paths this tool wrote.
safe_parents() {
    local rel=$1 dir=${SYSROOT%/} part owner mode
    local -a parts
    IFS=/ read -r -a parts <<<"$rel"
    unset 'parts[${#parts[@]}-1]'
    for part in "" "${parts[@]}"; do
        [[ -z $part ]] || dir+=/$part
        [[ -n $dir ]] || dir=/
        [[ -e $dir || -L $dir ]] || return 0
        if [[ -L $dir || ! -d $dir ]]; then
            warn "/${dir#"${SYSROOT%/}"} is not a real directory; refusing to change anything below it"
            return 1
        fi
        read -r owner mode < <(stat -c '%u %a' "$dir")
        if [[ $owner != "$OWNER_UID" ]] || (( 8#$mode & 8#022 )); then
            warn "/${dir#"${SYSROOT%/}"} is not root-owned and closed to group and others (owner $owner, mode $mode); refusing"
            return 1
        fi
    done
}

# Abort before any change unless every path in "$@" is reachable safely.
require_safe_paths() {
    local rel failed=0
    for rel in "$@"; do safe_parents "$rel" || failed=1; done
    (( ! failed )) || die "unsafe directory on a path this tool manages; nothing was changed"
}

# One operator command at a time. flock is released by the kernel when the
# process ends, so an interrupted command never leaves a stale lock.
operator_lock() {
    local wait=$1 path
    command -v flock >/dev/null || die "flock (util-linux) is required"
    if (( TEST_MODE )) && [[ ! -d $(at run) ]]; then install -d -m 0755 "$(at run)"; fi
    require_safe_paths "$LOCK"
    path=$(at "$LOCK")
    [[ ! -L $path ]] || die "/$LOCK is a symlink; refusing"
    exec {LOCK_FD}>>"$path"
    [[ ! -L $path && -f $path ]] || die "/$LOCK is not a regular file; refusing"
    # shellcheck disable=SC2086 # $wait is one flock option
    flock $wait "$LOCK_FD" \
        || die "another remote-provider-operator command holds /$LOCK; nothing was changed"
    # Leftovers of an interrupted ledger rewrite; the lock makes them stale.
    rm -f "$(at "$LEDGER")"/.*.tmp "$(at "$LEDGER/instances")"/.*.tmp
    test_hold
}

# --- offline test hooks (test mode only) ---------------------------------

# Hold the lock until the test releases it, to exercise a concurrent run.
test_hold() {
    (( TEST_MODE )) && [[ -n ${FLOWERLY_OPERATOR_TEST_HOLD:-} ]] || return 0
    : >"$FLOWERLY_OPERATOR_TEST_HOLD.held"
    local _
    for _ in $(seq 1 400); do
        [[ -e $FLOWERLY_OPERATOR_TEST_HOLD.release ]] && return 0
        sleep 0.05
    done
    die "test hold was never released"
}

# Count one change; die like a power cut after the configured number, to
# show that every interruption point leaves a state a rerun completes.
TEST_STEPS=0
test_step() {
    (( TEST_MODE )) && [[ -n ${FLOWERLY_OPERATOR_TEST_CRASH_AFTER:-} ]] || return 0
    TEST_STEPS=$((TEST_STEPS + 1))
    (( TEST_STEPS < FLOWERLY_OPERATOR_TEST_CRASH_AFTER )) || kill -KILL $$
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
ledger_add_shared() { printf '%s\t%s\t%s\n' "$1" "$2" "${3:--}" >>"$(ledger_shared)"; test_step; }
# Rewrite every entry `from<TAB>name<TAB>…` of a ledger as `to<TAB>name<TAB>…`.
ledger_rekind() {
    local ledger=$1 from=$2 to=$3 name=$4 tmp
    tmp="${ledger%/*}/.${ledger##*/}.tmp"
    awk -F'\t' -v OFS='\t' -v f="$from" -v t="$to" -v n="$name" '$1 == f && $2 == n { $1 = t } { print }' "$ledger" >"$tmp"
    mv "$tmp" "$ledger"
    test_step
}
# Rewrite a ledger without one entry. The rename is atomic, so an
# interruption leaves either the old or the new ledger, never a torn one.
ledger_drop() {
    local ledger=$1 kind=$2 name=$3 tmp
    tmp="${ledger%/*}/.${ledger##*/}.tmp"
    awk -F'\t' -v k="$kind" -v n="$name" '!($1 == k && $2 == n)' "$ledger" >"$tmp"
    mv "$tmp" "$ledger"
    test_step
}

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

# Where a file is staged next to its target before it is linked into place.
staged_of() { printf '%s/.%s.flowerly-new' "$(dirname "$1")" "$(basename "$1")"; }

# Create /rel from source, never replacing anything, and record it so that
# an interruption at any point is recoverable without ever taking another
# installation's file for ours:
#   1. `pending` entry;  2. write the staged copy;
#   3. hard-link it to the target (fails if the target exists);
#   4. entry becomes `file`;  5. remove the staged copy.
# Until step 4 the target counts as ours only if it is the same inode as
# our staged copy, which nobody else can produce.
install_recorded() {
    local ledger=$1 rel=$2 source=$3 mode=$4 target staged
    target=$(at "$rel")
    staged=$(staged_of "$target")
    printf 'pending\t%s\t%s\n' "$rel" "$(sha "$source")" >>"$ledger"
    test_step
    install -d -m 0755 "$(dirname "$target")"
    rm -f "$staged"
    install -m "$mode" "$source" "$staged"
    test_step
    ln "$staged" "$target" 2>/dev/null || die "/$rel appeared during install; it was not replaced (run rollback)"
    test_step
    ledger_rekind "$ledger" pending file "$rel"
    rm -f "$staged"
    test_step
}

install_instance() {
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
        [[ ! -e $(at "$rel") && ! -L $(at "$rel") ]] \
            || die "refusing to overwrite /$rel; it was not installed by this tool for $instance"
    done < <(instance_files "$instance")
    local -a targets
    mapfile -t targets < <(instance_files "$instance")
    require_safe_paths "${targets[@]}" "$UNIT" "$SYSUSERS" "$BIN" "$CLIENT" "$LEDGER/instances/$instance"
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

    # Every change is recorded so an interrupted install leaves a ledger
    # that `rollback` (or `rollback-shared`) finishes, and nothing recorded
    # is ever mistaken for another installation's resource.
    install -d -m 0700 "$(at "$LEDGER")" "$(at "$LEDGER/instances")"
    for rel in "$UNIT" "$SYSUSERS" "$BIN" "$CLIENT"; do
        [[ ${plan[$rel]} == create ]] || continue
        local mode=0644
        [[ $rel == "$BIN" || $rel == "$CLIENT" ]] && mode=0755
        install_recorded "$(ledger_shared)" "$rel" "${source[$rel]}" "$mode"
    done
    # Accounts are recorded `pending-*` before systemd-sysusers runs and
    # confirmed after it. An entry still pending after an interruption
    # cannot be told apart from an account someone else created meanwhile,
    # so rollback never deletes it (see rollback_shared).
    local -a created=()
    local item kind name
    [[ -n $(passwd_entry "$PROVIDER_USER") ]] || created+=("user $PROVIDER_USER")
    [[ -n $(group_entry "$PROVIDER_USER") ]] || created+=("group $PROVIDER_USER")
    [[ -n $(group_entry "$CLIENT_GROUP") ]] || created+=("group $CLIENT_GROUP")
    for item in "${created[@]}"; do
        read -r kind name <<<"$item"
        ledger_add_shared "pending-$kind" "$name"
    done
    system systemd-sysusers "/$SYSUSERS"
    test_step
    for item in "${created[@]}"; do
        read -r kind name <<<"$item"
        if [[ $kind == user ]]; then [[ -n $(passwd_entry "$name") ]] || continue
        else [[ -n $(group_entry "$name") ]] || continue; fi
        ledger_rekind "$(ledger_shared)" "pending-$kind" "$kind" "$name"
    done
    check_identities

    while read -r rel; do
        install_recorded "$(ledger_instance "$instance")" "$rel" "$staging/$rel" 0644
    done < <(instance_files "$instance")
    system systemctl daemon-reload
    echo "installed $(unit_of "$instance"): not started, not enabled; nothing was sent"
}

# --- rollback ------------------------------------------------------------

# Remove a file only if it is still the one this tool wrote, reached
# through directories only root can change. An absent file counts as
# removed, so a rerun after an interruption converges.
remove_if_unmodified() {
    local rel=$1 expected=$2 target
    target=$(at "$rel")
    safe_parents "$rel" || { warn "kept /$rel"; return 1; }
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
    test_step
}

# Finish one ledger file entry: `file` entries are ours (the link
# succeeded); a `pending` target is ours only if it is the same inode as
# our staged copy. A staged copy left by an interruption always goes.
remove_recorded() {
    local kind=$1 rel=$2 expected=$3 target staged
    target=$(at "$rel")
    staged=$(staged_of "$target")
    safe_parents "$rel" || { warn "kept /$rel"; return 1; }
    if [[ $kind == pending ]]; then
        if [[ -f $staged && ! -L $staged && ! -L $target && $target -ef $staged ]]; then
            rm -f "$target"
            test_step
        elif [[ -e $target || -L $target ]]; then
            warn "kept /$rel: an interrupted install never created it"
        fi
    else
        remove_if_unmodified "$rel" "$expected" || return 1
    fi
    if [[ -L $staged ]]; then rm -f "$staged"
    elif [[ -e $staged ]]; then rm -f "$staged"; test_step; fi
}

validate_shared_ledger() {
    local kind rel expected
    while IFS=$'\t' read -r kind rel expected; do
        case "$kind:$rel" in
            file:*|pending:*) [[ $rel == "$UNIT" || $rel == "$SYSUSERS" || $rel == "$BIN" || $rel == "$CLIENT" ]] \
                && [[ $expected =~ ^[0-9a-f]{64}$ ]] ;;
            "user:$PROVIDER_USER"|"group:$PROVIDER_USER"|"group:$CLIENT_GROUP") [[ $expected == - ]] ;;
            "pending-user:$PROVIDER_USER"|"pending-group:$PROVIDER_USER"|"pending-group:$CLIENT_GROUP") [[ $expected == - ]] ;;
            *) false ;;
        esac || die "shared ledger names an unexpected entry ($kind $rel); nothing removed"
    done <"$(ledger_shared)"
    require_safe_paths "$LEDGER/shared" "$UNIT" "$SYSUSERS" "$BIN" "$CLIENT"
}

# Everything that may still use the shared resources, one line each. A
# check that cannot complete is reported as a user too, so it refuses the
# cleanup instead of passing as "unused".
shared_consumers() {
    local instance=$1 path name unit units pids status member group
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
    if units=$(active_provider_units); then
        while read -r unit; do
            [[ -z $unit || $unit == "$(unit_of "$instance")" ]] || echo "active unit $unit"
        done <<<"$units"
    else
        echo "unknown: active provider units could not be listed"
    fi
    status=0
    provider_has_processes "$PROVIDER_USER" || status=$?
    case $status in
        0) echo "processes running as $PROVIDER_USER" ;;
        1) ;;
        *) echo "unknown: processes of $PROVIDER_USER could not be listed" ;;
    esac
    if pids=$(binary_users); then
        [[ -z $pids ]] || echo "processes executing /$BIN: $pids"
    else
        echo "unknown: processes executing /$BIN could not be listed"
    fi
    for group in "$PROVIDER_USER" "$CLIENT_GROUP"; do
        shared_owned group "$group" || continue
        while read -r member; do
            [[ -z $member || $member == "$PROVIDER_USER" ]] || echo "member $member of group $group"
        done < <(group_members "$group")
    done
}

# Remove the shared resources this tool created. Every usage check runs
# first; if anything may still use them, nothing shared is changed. Exit 0
# when they were kept only for other installations, which is expected.
rollback_shared() {
    local instance=${1:-} consumers line ledger status=0 kind name expected
    consumers=$(shared_consumers "$instance")
    if [[ -n $consumers ]]; then
        echo "kept the shared unit template, binary, client, account and groups; still used by:"
        while read -r line; do echo "  $line"; done <<<"$consumers"
        if grep -qv -e '^instance ' -e '^provider config ' -e '^active unit ' <<<"$consumers"; then
            warn "shared resources unchanged; run rollback-shared once nothing above uses them"
            return 1
        fi
        return 0
    fi
    ledger=$(ledger_shared)
    local -a entries
    mapfile -t entries <"$ledger"
    for line in "${entries[@]}"; do
        IFS=$'\t' read -r kind name expected <<<"$line"
        [[ $kind == file || $kind == pending ]] || continue
        if remove_recorded "$kind" "$name" "$expected"; then ledger_drop "$ledger" "$kind" "$name"; else status=1; fi
    done
    # Accounts an interrupted install recorded but never confirmed: they
    # may be someone else's, so they are reported, never deleted.
    for line in "${entries[@]}"; do
        IFS=$'\t' read -r kind name expected <<<"$line"
        case $kind in
            pending-user) [[ -z $(passwd_entry "$name") ]] \
                || { warn "kept account $name: an interrupted install may not have created it; remove it by hand only if nothing else uses it"; status=1; } ;;
            pending-group) [[ -z $(group_entry "$name") ]] \
                || { warn "kept group $name: an interrupted install may not have created it; remove it by hand only if nothing else uses it"; status=1; } ;;
            *) continue ;;
        esac
        ledger_drop "$ledger" "$kind" "$name"
    done
    if shared_owned user "$PROVIDER_USER"; then
        if [[ -n $(passwd_entry "$PROVIDER_USER") ]]; then
            system userdel "$PROVIDER_USER"
            test_step
        fi
        ledger_drop "$ledger" user "$PROVIDER_USER"
    fi
    for name in "$PROVIDER_USER" "$CLIENT_GROUP"; do
        shared_owned group "$name" || continue
        if [[ -n $(group_entry "$name") ]]; then
            # An account this tool did not create keeps its groups.
            if [[ -n $(passwd_entry "$PROVIDER_USER") ]]; then
                warn "kept group $name: account $PROVIDER_USER, not created by this tool, still exists"
                status=1
                continue
            fi
            system groupdel "$name"
            test_step
        fi
        ledger_drop "$ledger" group "$name"
    done
    rmdir "$(at "$LIBEXEC")" 2>/dev/null || true
    [[ -s $ledger ]] || rm -f "$ledger"
    return $status
}

rollback() {
    [[ $# -eq 1 ]] || usage
    local instance=$1 unit status=0 kind rel expected
    valid_instance "$instance"
    unit=$(unit_of "$instance")
    local ledger
    ledger=$(ledger_instance "$instance")
    [[ -f $ledger ]] || die "no install ledger for $instance: this tool did not install it, or an earlier rollback finished it (then use rollback-shared); nothing is removed"
    # Validate the whole ledger and every path before touching anything.
    local -A allowed=()
    local -a paths=("$LEDGER/instances/$instance" "$RUNTIME" "run/systemd/system/$unit.d/50-discover-only.conf")
    while read -r rel; do allowed[$rel]=1; done < <(instance_files "$instance")
    while IFS=$'\t' read -r kind rel expected; do
        [[ ($kind == file || $kind == pending) && -n ${allowed[$rel]:-} && $expected =~ ^[0-9a-f]{64}$ ]] \
            || die "ledger for $instance names an unexpected entry ($kind $rel); nothing removed"
        paths+=("$rel")
    done <"$ledger"
    require_safe_paths "${paths[@]}"
    [[ ! -L $(at "$RUNTIME") ]] || die "/$RUNTIME is a symlink; nothing removed"
    [[ ! -f $(ledger_shared) ]] || validate_shared_ledger

    system systemctl stop "$unit" || true
    system systemctl unmask "$unit" || true
    rm -f "$(at "run/systemd/system/$unit.d/50-discover-only.conf")"
    rmdir "$(at "run/systemd/system/$unit.d")" 2>/dev/null || true
    # This instance's runtime files only, never the shared directory. The
    # provider owns that directory, but rm never follows a final symlink.
    for rel in "$RUNTIME/$instance.sock" "$RUNTIME/$instance.sock.lock"; do
        if [[ -L $(at "$rel") ]]; then warn "kept /$rel: a symlink"; status=1
        elif [[ -e $(at "$rel") ]]; then rm -f "$(at "$rel")"; test_step; fi
    done
    # Each removed file leaves the ledger; a kept one stays recorded.
    local line
    local -a entries
    mapfile -t entries <"$ledger"
    for line in "${entries[@]}"; do
        IFS=$'\t' read -r kind rel expected <<<"$line"
        if remove_recorded "$kind" "$rel" "$expected"; then ledger_drop "$ledger" "$kind" "$rel"; else status=1; fi
    done
    rmdir "$(at "etc/systemd/system/$unit.d")" 2>/dev/null || true

    if [[ -s $ledger ]]; then
        warn "kept the install ledger of $instance for the files above; restore or remove them, then rerun: rollback $instance"
        echo "kept the shared unit template, binary, client, account and groups: $instance is not fully rolled back"
        status=1
    else
        rm -f "$ledger"
        test_step
        if [[ -f $(ledger_shared) ]]; then rollback_shared "$instance" || status=1; fi
    fi
    rmdir "$(at "$RUNTIME")" "$(at etc/flowerly/providers)" "$(at etc/flowerly)" \
        "$(at "$LEDGER/instances")" "$(at "$LEDGER")" 2>/dev/null || true
    system systemctl daemon-reload
    if (( status )); then
        echo "rollback of $unit is incomplete; see the messages above"
    else
        echo "rolled back $unit; /etc/credstore.encrypted/$instance.key was left for the operator to remove"
    fi
    return $status
}

# Shared resources kept by an earlier rollback, once nothing uses them.
rollback_shared_command() {
    [[ $# -eq 0 ]] || usage
    [[ -f $(ledger_shared) ]] || die "no shared resources recorded by this tool"
    validate_shared_ledger
    local status=0
    rollback_shared "" || status=1
    [[ ! -f $(ledger_shared) ]] || status=1
    rmdir "$(at "$RUNTIME")" "$(at etc/flowerly/providers)" "$(at etc/flowerly)" \
        "$(at "$LEDGER/instances")" "$(at "$LEDGER")" 2>/dev/null || true
    system systemctl daemon-reload
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

# Installed by this tool, completely and unmodified: a partly rolled back
# or edited instance is never started.
installed_here() {
    local ledger kind rel expected
    ledger=$(ledger_instance "$1")
    [[ -f $ledger ]] || die "instance $1 was not installed by this tool"
    [[ $(wc -l <"$ledger") -eq 3 ]] || die "instance $1 is not completely installed; roll it back first"
    while IFS=$'\t' read -r kind rel expected; do
        [[ $kind == file ]] || die "instance $1 is not completely installed; roll it back first"
        safe_parents "$rel" || die "instance $1: unsafe path /$rel"
        [[ -f $(at "$rel") && ! -L $(at "$rel") && $(sha "$(at "$rel")") == "$expected" ]] \
            || die "instance $1: /$rel is missing or modified since install; roll it back first"
    done <"$ledger"
    check_identities
}

preflight() {
    [[ $# -ge 1 && $# -le 2 ]] || usage
    local instance=$1 dir=${2:-/var/log/flowerly-provider-acceptance} unit runtime status=0
    valid_instance "$instance"
    installed_here "$instance"
    no_test_mode preflight
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
    [[ $# -ge 2 && $# -le 3 ]] || usage
    local instance=$1 approval=$2 dir=${3:-/var/log/flowerly-provider-acceptance} unit status=0
    valid_instance "$instance"
    installed_here "$instance"
    no_test_mode accept
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
    # cannot be started again until unmasked. It comes before the lock: it
    # only ever restricts, and it must not wait for a running command.
    systemctl mask --now "$unit"
    operator_lock "-w 600"
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
    install|preflight|accept|rollback|rollback-shared)
        need_root
        operator_lock -n ;;
esac
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

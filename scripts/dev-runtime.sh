#!/usr/bin/env bash
# Flowerly OS local development runtime. PID records are scoped to RUNTIME_DIR.
set -euo pipefail
umask 077

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$REPO_ROOT/target/release}"
RUNTIME_DIR="${RUNTIME_DIR:-${XDG_RUNTIME_DIR:-$REPO_ROOT/target}/flowerly-dev}"
STATE_DIR="${STATE_DIR:-$REPO_ROOT/target/flowerly-state}"
SERVICES=(agenticd flowd sessiond goald provenanced capabilityd generationd evolutiond)

mkdir -p -m 700 "$RUNTIME_DIR" "$STATE_DIR"
chmod 700 "$RUNTIME_DIR" "$STATE_DIR"
RUNTIME_DIR="$(cd "$RUNTIME_DIR" && pwd -P)"
STATE_DIR="$(cd "$STATE_DIR" && pwd -P)"
export AGENTIC_RUNTIME_DIR="$RUNTIME_DIR"
export AGENTIC_STATE_DIR="$STATE_DIR"
export AGENTIC_LOG_FORMAT="${AGENTIC_LOG_FORMAT:-text}"

ctl() {
    "$BIN_DIR/agenticctl" --runtime-dir "$RUNTIME_DIR" "$@"
}

# /proc/pid/stat field 22 identifies the process incarnation (PID reuse).
start_ticks() {
    local stat
    [[ -r "/proc/$1/stat" ]] || return 1
    stat="$(cat "/proc/$1/stat")"
    stat="${stat##*) }"
    awk '{print $20}' <<< "$stat"
}

is_owned() {
    local service="$1" pid="$2" ticks="$3" observed env_path
    [[ "$pid" =~ ^[0-9]+$ && -n "$ticks" ]] || return 1
    observed="$(start_ticks "$pid")" || return 1
    [[ "$observed" == "$ticks" ]] || return 1
    [[ "$(cat "/proc/$pid/comm" 2>/dev/null)" == "$service" ]] || return 1
    # Recheck the environment: a matching process name is not ownership.
    env_path="$(tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -Fx "AGENTIC_RUNTIME_DIR=$RUNTIME_DIR" || true)"
    [[ -n "$env_path" ]]
}

read_owned_pid() {
    local service="$1" pid ticks
    [[ -f "$RUNTIME_DIR/$service.pid" ]] || return 1
    read -r pid ticks < "$RUNTIME_DIR/$service.pid" || return 1
    if is_owned "$service" "$pid" "$ticks"; then
        printf '%s\n' "$pid"
    else
        return 1
    fi
}

ready() {
    local service="$1"
    [[ -S "$RUNTIME_DIR/$service.sock" ]] && ctl status "$service" >/dev/null 2>&1
}

start() {
    local service pid ticks running ok
    for service in "${SERVICES[@]}"; do
        if running="$(read_owned_pid "$service")"; then
            if ready "$service"; then
                echo "already running $service ($running)"
                continue
            fi
            echo "error: owned $service is alive but not ready" >&2
            return 1
        fi
        if [[ -e "$RUNTIME_DIR/$service.sock" || -L "$RUNTIME_DIR/$service.sock" ]]; then
            echo "error: unowned socket at $RUNTIME_DIR/$service.sock; inspect before recovery" >&2
            return 1
        fi
        rm -f "$RUNTIME_DIR/$service.pid"
        "$BIN_DIR/$service" >"$RUNTIME_DIR/$service.log" 2>&1 &
        pid=$!
        ticks=""
        for _ in $(seq 1 20); do
            ticks="$(start_ticks "$pid" || true)"
            [[ -n "$ticks" ]] && break
            sleep 0.05
        done
        [[ -n "$ticks" ]] || { echo "error: unable to identify $service process" >&2; return 1; }
        printf '%s %s\n' "$pid" "$ticks" > "$RUNTIME_DIR/$service.pid"
        ok=0
        for _ in $(seq 1 50); do
            if ready "$service"; then ok=1; break; fi
            if ! kill -0 "$pid" 2>/dev/null; then break; fi
            sleep 0.1
        done
        if (( ! ok )); then
            echo "error: $service failed readiness; see $RUNTIME_DIR/$service.log" >&2
            return 1
        fi
        echo "started $service ($pid)"
    done
}

stop() {
    local service pid ticks i
    for service in "${SERVICES[@]}"; do
        if ! pid="$(read_owned_pid "$service")"; then
            echo "skip $service (no owned process)"
            continue
        fi
        read -r _ ticks < "$RUNTIME_DIR/$service.pid"
        kill -TERM "$pid" 2>/dev/null || true
        for i in $(seq 1 25); do
            is_owned "$service" "$pid" "$ticks" || break
            sleep 0.2
        done
        if is_owned "$service" "$pid" "$ticks"; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
        rm -f "$RUNTIME_DIR/$service.pid"
        echo "stopped $service ($pid)"
    done
}

status() {
    local service
    for service in "${SERVICES[@]}"; do
        echo "== $service =="
        if ! ready "$service"; then
            echo "unavailable or not ready" >&2
            return 1
        fi
        ctl status "$service"
    done
}

case "${1:-}" in
    start) start ;;
    stop) stop ;;
    restart) stop; start ;;
    status) status ;;
    *) echo "usage: $0 {start|stop|restart|status}" >&2; exit 2 ;;
esac

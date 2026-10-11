#!/usr/bin/env bash
# Flowerly OS — local development runtime.
#
# Starts every daemon against a dedicated runtime directory, waits for their
# IPC sockets, and reports status through agenticctl. Used by `just serve`.
#
#   start   launch all services
#   stop    terminate all services (graceful SIGTERM, then SIGKILL)
#   status  print `agenticctl status` for every service
#   restart stop + start
#
# The runtime directory defaults to $XDG_RUNTIME_DIR/flowerly-dev and can be
# overridden with RUNTIME_DIR. Binaries are resolved from $BIN_DIR (default
# target/release of the repository root).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${BIN_DIR:-$REPO_ROOT/target/release}"
RUNTIME_DIR="${RUNTIME_DIR:-${XDG_RUNTIME_DIR:-$REPO_ROOT/target}/flowerly-dev}"

SERVICES=(agenticd flowd sessiond goald provenanced capabilityd generationd evolutiond)

mkdir -p "$RUNTIME_DIR"
export AGENTIC_RUNTIME_DIR="$RUNTIME_DIR"
export AGENTIC_LOG_FORMAT="${AGENTIC_LOG_FORMAT:-text}"

ctl() {
    "$BIN_DIR/agenticctl" --runtime-dir "$RUNTIME_DIR" "$@"
}

start() {
    for service in "${SERVICES[@]}"; do
        if [[ -S "$RUNTIME_DIR/$service.sock" ]]; then
            echo "skip $service (already running)"
            continue
        fi
        "$BIN_DIR/$service" >"$RUNTIME_DIR/$service.log" 2>&1 &
        echo "start $service (pid $!)"
    done

    # Wait for sockets to appear (bounded).
    for service in "${SERVICES[@]}"; do
        for _ in $(seq 1 50); do
            [[ -S "$RUNTIME_DIR/$service.sock" ]] && break
            sleep 0.1
        done
        if [[ ! -S "$RUNTIME_DIR/$service.sock" ]]; then
            echo "warning: $service did not create a socket" >&2
        fi
    done

    status
}

stop() {
    local pids=()
    local pgid
    for service in "${SERVICES[@]}"; do
        pgid="$(pgrep -x "$service" || true)"
        if [[ -n "$pgid" ]]; then
            pids+=("$pgid")
        fi
    done
    if [[ ${#pids[@]} -eq 0 ]]; then
        echo "no services running"
        return 0
    fi
    kill -TERM "${pids[@]}" 2>/dev/null || true
    for _ in $(seq 1 25); do
        local remaining=0
        for pid in "${pids[@]}"; do
            if kill -0 "$pid" 2>/dev/null; then
                remaining=1
            fi
        done
        [[ $remaining -eq 0 ]] && break
        sleep 0.2
    done
    kill -KILL "${pids[@]}" 2>/dev/null || true
    echo "stopped services: ${SERVICES[*]}"
}

status() {
    for service in "${SERVICES[@]}"; do
        if [[ -S "$RUNTIME_DIR/$service.sock" ]]; then
            echo "== $service =="
            "$BIN_DIR/agenticctl" --runtime-dir "$RUNTIME_DIR" status "$service" || true
        else
            echo "== $service == (not running)"
        fi
    done
}

case "${1:-}" in
    start) start ;;
    stop) stop ;;
    restart) stop; start ;;
    status) status ;;
    *) echo "usage: $0 {start|stop|restart|status}"; exit 2 ;;
esac
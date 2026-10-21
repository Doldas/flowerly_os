#!/usr/bin/env bash
# Emit a boot witness only after a successful Flowerly IPC Ping.
set -euo pipefail
AGENTICCTL="${FLOWERLY_BOOT_AGENTICCTL:-/usr/bin/agenticctl}"
ATTEMPTS="${FLOWERLY_BOOT_PING_ATTEMPTS:-30}"
case "$ATTEMPTS" in
    ''|*[!0-9]*) echo "invalid boot ping attempts" >&2; exit 1 ;;
esac
if (( ATTEMPTS < 1 || ATTEMPTS > 120 )); then
    echo "boot ping attempts must be 1..120" >&2
    exit 1
fi
for (( i=1; i<=ATTEMPTS; i++ )); do
    if "$AGENTICCTL" --runtime-dir /run/flowerly ping flowerlyd >/dev/null 2>&1; then
        echo FLOWERLY_BOOT_IPC_HEALTHY
        exit 0
    fi
    if (( i < ATTEMPTS )); then sleep 1; fi
done
echo "flowerlyd did not respond to IPC Ping after $ATTEMPTS attempts" >&2
exit 1

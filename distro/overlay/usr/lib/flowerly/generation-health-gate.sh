#!/usr/bin/env bash
# Test-image-only health gate. Never bless a boot until the daemon IPC Ping
# and Generation root witness have both passed.
set -euo pipefail
cmdline="$(cat /proc/cmdline)"
if [[ " $cmdline " == *" flowerly.test.fail_health=1 "* ]]; then
    if [[ ! -f /usr/lib/flowerly/generation-ab-test-profile ]]; then
        echo "refusing health fault switch outside signed-off test profile" >&2
        exit 1
    fi
    echo "injected unhealthy Generation boot: intentionally leaving boot unblessed" >&2
    exit 1
fi
if [[ ! -f /var/lib/flowerly/boot-witness.jsonl ]]; then
    echo "no fsynced Generation boot witness on persistent data partition" >&2
    exit 1
fi
if ! command -v runuser >/dev/null 2>&1; then
    echo "cannot perform service-account IPC Ping without runuser" >&2
    exit 1
fi
# Do not let a privileged root IPC Ping bypass the service-account admission
# boundary. The existing script must succeed as the Flowerly service user.
runuser -u flowerly -- /usr/bin/bash /usr/lib/flowerly/boot-ipc-health.sh
echo "FLOWERLY_GENERATION_IPC_VERIFIED"

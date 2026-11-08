#!/usr/bin/env bash
# Test-image-only. Runs after the health gate passed. systemd-bless-boot only
# applies to an entry the loader is still counting; an entry that was already
# blessed (for example slot A after an automatic fallback) has no counter and
# the boot is already good, so there is nothing to bless.
set -euo pipefail
counter=/sys/firmware/efi/efivars/LoaderBootCountPath-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f
if [[ -e "$counter" ]]; then
    exec /usr/lib/systemd/systemd-bless-boot good
fi
echo "booted entry has no boot counter; it is already good, nothing to bless"

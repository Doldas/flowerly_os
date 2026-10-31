#!/usr/bin/env bash
# Power-cut interposer for the KVM guest's lifecycle tests (#263). TEST ONLY.
#
# Installed by the guest runner as <dir>/<command> symlinks to this file,
# with <dir> first in PATH for ONE operator invocation. It runs the real
# /usr/bin/<command>, counts the call in $FLOWERLY_KVM_INTERPOSE_COUNTER and,
# on call number $FLOWERLY_KVM_INTERPOSE_AFTER, kills its parent (the
# operator script) with SIGKILL, like a power cut right after that step.
# Refuses outside the disposable guest the runner prepared.
set -euo pipefail
name=$(basename "$0")
[[ -f /etc/flowerly-kvm-disposable-guest ]] || { echo "interpose: not a disposable KVM guest" >&2; exit 97; }
real=/usr/bin/$name
[[ -x $real && $real != "$0" ]] || { echo "interpose: no real $real" >&2; exit 97; }
status=0
"$real" "$@" || status=$?
counter=${FLOWERLY_KVM_INTERPOSE_COUNTER:?}
count=$(( $(cat "$counter" 2>/dev/null || echo 0) + 1 ))
echo "$count" >"$counter"
if [[ $count -eq ${FLOWERLY_KVM_INTERPOSE_AFTER:?} ]]; then
    kill -KILL "$PPID"
fi
exit $status

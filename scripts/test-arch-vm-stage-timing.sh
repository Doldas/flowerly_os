#!/usr/bin/env bash
# Offline regression checks for Arch VM stage evidence (#285).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="$(mktemp -d)"
trap 'rm -rf "$REPORT_DIR"' EXIT
source "$ROOT/scripts/lib/arch-vm-stage-timing.sh"
flowerly_stage_begin image_acquisition
flowerly_stage_started="$(( $(date +%s) - 3 ))"
flowerly_stage_begin guest_verification
flowerly_stage_started="$(( $(date +%s) - 2 ))"
flowerly_stage_finish interrupted
flowerly_stage_finish interrupted # Idempotent, not a duplicate row.
python3 - "$REPORT_DIR/host-stage-timings.tsv" <<'PY'
from pathlib import Path
import sys

rows = Path(sys.argv[1]).read_text().splitlines()
assert rows[0] == "stage\tduration_seconds\toutcome"
first = rows[1].split("\t")
second = rows[2].split("\t")
assert first[0] == "image_acquisition" and first[2] == "completed", rows
assert second[0] == "guest_verification" and second[2] == "interrupted", rows
assert 3 <= int(first[1]) <= 5, rows
assert 2 <= int(second[1]) <= 4, rows
assert len(rows) == 3, rows
print("arch VM stage ledger regression passed")
PY

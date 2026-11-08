#!/usr/bin/env bash
# Non-secret monotonic per-stage wall time evidence for Arch VM acceptance (#285).
# Source after REPORT_DIR exists. Stages must not contain dynamic user values.
flowerly_stage_file="$REPORT_DIR/host-stage-timings.tsv"
printf 'stage\tduration_seconds\toutcome\n' > "$flowerly_stage_file"
flowerly_stage_name=""
flowerly_stage_started=""

flowerly_stage_finish() {
    local outcome="${1:-completed}"
    [[ "$outcome" == completed || "$outcome" == interrupted ]] || { echo "invalid stage outcome" >&2; return 1; }
    [[ -n "$flowerly_stage_name" ]] || return 0
    local now elapsed
    now="$(date +%s)"
    elapsed=$((now - flowerly_stage_started))
    (( elapsed >= 0 )) || { echo "clock moved backwards during VM stage" >&2; return 1; }
    printf '%s\t%s\t%s\n' "$flowerly_stage_name" "$elapsed" "$outcome" >> "$flowerly_stage_file"
    flowerly_stage_name=""
    flowerly_stage_started=""
}

flowerly_stage_begin() {
    [[ "$1" =~ ^[a-z][a-z0-9_]*$ ]] || { echo "invalid stage name" >&2; return 1; }
    flowerly_stage_finish completed
    flowerly_stage_name="$1"
    flowerly_stage_started="$(date +%s)"
}

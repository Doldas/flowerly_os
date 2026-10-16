#!/usr/bin/env bash

set -euo pipefail

BIN_DIR="${1:-target/release}"
services=(flowerlyd evolutiond)

for service in "${services[@]}"; do
    binary="$BIN_DIR/$service"
    if [[ ! -x "$binary" ]]; then
        echo "missing executable: $binary" >&2
        exit 1
    fi

    echo "== $service --self-test =="
    "$binary" --self-test
done

echo "All advertised daemon self-tests passed."

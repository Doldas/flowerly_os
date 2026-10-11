#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
IMAGE_DIR="$ROOT/image/arch"
PACKAGES="$IMAGE_DIR/packages.base"
BINARIES="$IMAGE_DIR/flowerly-binaries.txt"
MKOSI="$IMAGE_DIR/mkosi.conf"

for path in "$PACKAGES" "$BINARIES" "$MKOSI"; do
    if [[ ! -s "$path" ]]; then
        echo "missing or empty image input: ${path#"$ROOT/"}" >&2
        exit 1
    fi
done

check_sorted_unique() {
    local path="$1"
    local label="$2"
    if ! diff -u "$path" <(LC_ALL=C sort -u "$path") >/dev/null; then
        echo "$label must be sorted and unique: ${path#"$ROOT/"}" >&2
        diff -u "$path" <(LC_ALL=C sort -u "$path") || true
        exit 1
    fi
}

check_sorted_unique "$PACKAGES" "Arch package inputs"
check_sorted_unique "$BINARIES" "Flowerly binary inputs"

if ! grep -Eq '^Distribution=arch$' "$MKOSI"; then
    echo "mkosi.conf must explicitly select Distribution=arch" >&2
    exit 1
fi

while IFS= read -r package; do
    [[ -z "$package" ]] && continue
    if ! grep -Eq "^[[:space:]]+${package}$" "$MKOSI"; then
        echo "package listed in packages.base but missing from mkosi.conf: $package" >&2
        exit 1
    fi
done < "$PACKAGES"

echo "Flowerly Arch image inputs are consistent."
echo "packages: $(grep -cve '^$' "$PACKAGES")"
echo "Flowerly binaries expected later: $(grep -cve '^$' "$BINARIES")"
echo "note: this validates image inputs; it does not claim a bootable image exists yet."

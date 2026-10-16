#!/usr/bin/env bash
# Build a root-level GitHub Pages artifact using an isolated Python environment.
# PEP 668 forbids pip installing into distro-managed Python on release runners.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SITE_DIR="$ROOT/site"

# mktemp ensures concurrent runners do not share or reuse a venv. The venv is
# short-lived; the pinned dependencies and the cached pip wheels remain stable.
VENV_DIR="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/flowerly-docs-venv.XXXXXXXX")"
trap 'rm -rf -- "$VENV_DIR"' EXIT

if ! python3 -m venv "$VENV_DIR"; then
    echo "Could not create docs Python venv; use actions/setup-python or install python3-venv." >&2
    exit 1
fi

"$VENV_DIR/bin/python" -m pip install \
    --disable-pip-version-check \
    -r "$ROOT/web-docs/requirements-docs.txt"

"$VENV_DIR/bin/python" -m mkdocs build \
    --config-file "$ROOT/web-docs/mkdocs.yml" \
    --site-dir "$SITE_DIR"

# An upload-pages-artifact job must never pass with a missing/empty site.
if [[ ! -s "$SITE_DIR/index.html" ]]; then
    echo "documentation build did not create $SITE_DIR/index.html" >&2
    exit 1
fi

echo "GitHub Pages site is ready: $SITE_DIR/index.html"

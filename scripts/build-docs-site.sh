#!/usr/bin/env bash
# Build the GitHub Pages artifact at the repository root, independently of
# MkDocs's config-relative site_dir behavior (web-docs/mkdocs.yml).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SITE_DIR="$ROOT/site"

mkdocs build \
    --config-file "$ROOT/web-docs/mkdocs.yml" \
    --site-dir "$SITE_DIR"

# An upload-pages-artifact job must never pass with a missing/empty site.
if [[ ! -s "$SITE_DIR/index.html" ]]; then
    echo "documentation build did not create $SITE_DIR/index.html" >&2
    exit 1
fi

echo "GitHub Pages site is ready: $SITE_DIR/index.html"

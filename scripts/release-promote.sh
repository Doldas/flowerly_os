#!/usr/bin/env bash
# Promote a release: tag master and build artifacts.
# Usage: scripts/release-promote.sh <version>
# Example: scripts/release-promote.sh 0.1.0

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <version>"
    echo "Example: $0 0.1.0"
    exit 1
fi

VERSION="$1"
TAG="v$VERSION"

if [[ -n $(git status --porcelain) ]]; then
    echo "Error: working tree is not clean; commit or stash changes first"
    exit 1
fi

git fetch origin master
git checkout master
git pull origin master

# Ensure we're on master and up to date
echo "Tagging $TAG on master..."
git tag -a "$TAG" -m "Release $VERSION"
git push origin "$TAG"

echo "Release $TAG pushed. GitHub Actions release workflow will build and publish artifacts."
echo "Monitor: https://github.com/Doldas/flowerly_os/actions/workflows/release.yml"
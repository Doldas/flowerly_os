#!/usr/bin/env bash
# Cut a new release branch from master.
# Usage: scripts/release-cut.sh <N>
# Example: scripts/release-cut.sh 2  # creates release/2 from master

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <release-number>"
    echo "Example: $0 2"
    exit 1
fi

N="$1"
BRANCH="release/$N"

if ! [[ "$N" =~ ^[0-9]+$ ]]; then
    echo "Error: release number must be a positive integer"
    exit 1
fi

git fetch origin master
git checkout -b "$BRANCH" origin/master
git push -u origin "$BRANCH"

echo "Created and pushed $BRANCH from origin/master"
echo "Open PRs against $BRANCH for the next release cycle"
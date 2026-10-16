#!/usr/bin/env bash
# Cut release/N from the latest existing release branch (not stale master).
# Usage: bash scripts/release-cut.sh <next-number>
set -euo pipefail

if [[ $# -ne 1 || ! "$1" =~ ^[1-9][0-9]*$ ]]; then
    echo "Usage: $0 <positive-next-release-number>" >&2
    exit 1
fi
number="$1"
branch="release/$number"
if [[ -n "$(git status --porcelain)" ]]; then
    echo "Error: working tree must be clean" >&2
    exit 1
fi

git fetch origin --prune
latest="$(
    git for-each-ref --format='%(refname:short)' refs/remotes/origin/release |
        sed -nE 's@^origin/release/([1-9][0-9]*)$@\1@p' |
        sort -n |
        tail -n 1
)"
if [[ -n "$latest" ]]; then
    if (( 10#$number != 10#$latest + 1 )); then
        echo "Next branch must be release/$((10#$latest + 1))" >&2
        exit 1
    fi
    source_ref="origin/release/$latest"
else
    [[ "$number" == "1" ]] || {
        echo "No release branch exists; first release must be release/1" >&2
        exit 1
    }
    source_ref="origin/master"
fi

if git show-ref --verify --quiet "refs/heads/$branch" ||
   git show-ref --verify --quiet "refs/remotes/origin/$branch"; then
    echo "$branch already exists" >&2
    exit 1
fi

git checkout -b "$branch" "$source_ref"
git push -u origin "$branch"
echo "Created $branch from $source_ref; automated forwarding will reconcile its PRs"

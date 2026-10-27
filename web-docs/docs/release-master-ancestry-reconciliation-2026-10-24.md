# Release/2 and master ancestry reconciliation — 2026-10-24

This note accompanies a **two-parent Git commit** on a reconciliation branch
targeting `release/2`, not a promotion or authorization to merge into `master`.

## Verified inputs

- Current `release/2` first parent: `febf75475956c42577cf9a5c0ceea2ae59a5ee38`.
- Current `master` second parent: `762b35448dd4cd47d0bdba9ef43d7c4eb4bb1ded`.
- The reconciled commit uses the **complete release/2 tree** as its base,
  adding only this note. No master-side file content is copied over release
  changes: existing release branch CI, security, and runtime files are retained.

Promotion PR #193 displayed conflicts in `.github/workflows/ci.yml`,
`scripts/security-alert-inventory.py`,
`scripts/test-workflow-action-pins.py`, and
`src/flowerlyd/src/generation/mod.rs`. These paths had newer, tested
release-side revisions. Ordinary conflict-marker editing on PR #193 could
overwrite those changes or obscure the shared ancestry problem.

## Required merge method

**Merge this reconciliation PR into `release/2` with "Create a merge
commit". Never squash or rebase this PR.** Squashing or rebasing drops the
second `master` parent, reintroducing the same future promotion conflict.
Do not force-push `release/2` or `master`.

After that merge, independently verify the ancestry:

```sh
git fetch origin master release/2
git merge-base --is-ancestor origin/master origin/release/2
```

The command must exit 0. Confirm the release tree retains all CI, security,
runtime and artifact changes; compare it with the reconciled candidate. Wait
for required exact-`release/2`-HEAD push CI (including executed real Arch
VM/PT​​Y acceptance), docs, security and CodeQL to pass.

If `master` moves before the reconciliation is merged, stop and regenerate
the two-parent commit using the newer `master` head: do not assume ancestry
is preserved. If `release/2` moves, rebase by making a **fresh** two-parent
reconciliation commit with the latest release first parent; do not blindly
merge a stale snapshot.

## Master promotion remains a separate decision

PR #193 stays draft until its own protection, promotion policy, source-head
evidence and human review are satisfied. **Never auto-merge into master.**
Neither creating this commit nor merging this PR is approval to promote.

## Repeat reconciliation — 2026-10-25

Promotion PR #206 became conflicted after `master` received the squashed
promotion `c7d6ef903a5f02e60342fef94530002627a3c198` (#193). That squash
commit is not an ancestor of `release/2`.

- Verified: the tree of `c7d6ef9` is **identical** to the `release/2` tree at
  `a1aeabc` (#203), so `master` holds no content that `release/2` lacks.
- The reconciliation commit is a `-s ours` merge. Its first parent is the
  current `release/2` and its second parent is `master` `c7d6ef9`. It keeps
  the complete `release/2` tree and adds only this note.
- The same rules apply: merge the reconciliation PR into `release/2` with
  **"Create a merge commit"** (never squash or rebase), then verify
  `git merge-base --is-ancestor origin/master origin/release/2`.

To avoid repeating this, the future promotion of #206 into `master` should
also use a merge commit, not a squash, so `release/2` stays a descendant of
`master`. That choice belongs to the maintainer.

## Repeat reconciliation — 2026-10-27 (0.0.4 promotion)

PR #206 was promoted to `master` as the squash commit
`2d719551ff5a14a9231f51a323d3a7c6f57bbd88` (2026-10-27). That commit has a
single parent, as the linear-history rule on `master` requires, and is not an
ancestor of `release/2`.

- Verified: the tree of `2d71955` is **identical** to the tested candidate
  `6b7f363` (`7b25fd5306832ab30dfe283e445d3a4612e1bedf`).
- This reconciliation is again a `-s ours` merge. Its first parent is
  `release/2` `6b7f363` and its second parent is `master` `2d71955`. It keeps
  the complete `release/2` tree and adds only this note.
- Merge it into `release/2` with **"Create a merge commit"**, never squash or
  rebase. Then verify
  `git merge-base --is-ancestor origin/master origin/release/2`.

From now on, every promotion into `master` uses **squash**, because of the
linear-history rule, and is followed by this reconciliation into `release/2`.

# Flowerly OS — Releases

Flowerly OS uses semantic-ish pre-1.0 versioning. Pre-1.0 does **not** mean
careless: every release is tagged, described and backed by evidence, and known
limitations are stated explicitly rather than hidden.

## Promotion vs publication — verified 0.0.4 state (2026-10-27)

**Release implementation has been promoted; version 0.0.4 has not been published.**
The accepted release-branch candidate `6b7f363` passed real Arch-guest
push CI [40507915398](https://github.com/Doldas/flowerly_os/actions/runs/40507915398)
and dispatch [40507977240](https://github.com/Doldas/flowerly_os/actions/runs/40507977240).
Promotion PR #206 was squash-merged into `master` as
`2d719551ff5a14a9231f51a323d3a7c6f57bbd88`, preserving the required
linear history. The tree was verified to match the accepted candidate.

Subsequent `release/2` work reconciled ancestry (#250) and fixed a test-only
systemd restart limit flake (#251), IPC test client short reads (#253), and
promotion validation / required policy-check reporting (#254). After these
changes `release/2` was observed at `c19f9e1`, with draft promotion PR
#255. A new master promotion requires exact-SHA release push CI, executed
Arch acceptance, security, CodeQL, mandatory PR checks and explicit maintainer
authorization; the previous candidate evidence does not cover later commits.

**Neither a tag nor a GitHub Release is authorized merely by promotion.**
Before `v0.0.4` publication, obtain a separate maintainer decision and
verify the tagged source, its current acceptance evidence, release notes and
build manifests. Document the shipped systemd unit's absent `Delegate=`
(default PTY spawning disabled), service-account-only admission by default,
optional multi-user drop-in, and upgrade/rollback requirements.

See [completion plan](release-0.0.4-completion-plan.md) and
[engineering handoff](engineering-handoff-2026-10-27.md) for the current
administrative backlog and incident evidence.

## Version model

- `0.0.x` — foundational internal capability. Sprints that build the substrate:
  repository foundation, core runtime skeleton, persistent state, flows and
  sessions, generations, evolution lab, goals, agents.
- `0.1.0` — the first coherent, real Flowerly OS development release. It
  demonstrates a bootable system with persistent state, the General Flow,
  persistent sessions, goals, provenance, generations and rollback, the
  Evolution World, a provider-neutral agent runtime and one bounded
  self-development demonstration.

Post-0.1 direction (Flow OS, adaptive shell, ambient interaction, user model,
autonomous evolution, multi-machine) is described in the grounding specification
`AGENTIC_OS_REPOSITORY_ARCHITECTURE_SPRINTS_AND_OPENCODE.md`.

## Required release contents

Every release contains:

```text
source tag            an annotated Git tag on master
release notes         what changed, and what is now possible
build manifest        the exact inputs and toolchain used
test result summary   which suites passed, and any skipped
known limitations     incomplete or unverified behavior, stated plainly
artifact checksums    checksums of every published artifact
```

## Release branch model

Flowerly uses **forward-only release propagation** while keeping `master`
as the **latest verified and explicitly approved release**. Direct pushes to
protected release branches and `master` remain forbidden.

```text
feature/* / bugfix/* / docs/* -> release/2 -> release/3 -> release/4
                                                    |
                                   verified release-push CI
                                   (real Arch VM + docs + security + CodeQL)
                                                    |
                                   draft release/4 -> master PR
                                   (maintainer promotion approval)
                                                    v
                                                 master
```

- `scripts/release-cut.sh N` creates the next release from the **highest
  existing `release/N-1`**, not stale `master`. It requires consecutive
  numbering, a clean worktree and a new branch.
- Any protected `release/*` push runs
  `.github/workflows/branch-forward.yml`, which reconciles **all adjacent
  release branches** and opens idempotent forward PRs (`release/N` into
  `release/N+1`). It never pushes directly to a target branch.
- When GitHub's **Allow auto-merge** repository setting is enabled, the bot
  requests a **merge commit** for adjacent-release PRs. GitHub merges them
  only after the mandatory PR checks pass; conflicts leave the PR blocked.
  Without that setting, forward PRs remain open for manual merge.
- The same automation opens a **draft** PR from the **newest release branch**
  to `master`. It never enables master auto-merge. The PR-policy check
  rejects older release sources and requires successful **exact-head**
  release-branch **push** CI (including the *executed and passed* Arch VM
  acceptance step), security and CodeQL checks. Passing PR CI alone is
  insufficient; a failed, stale or skipped Arch step blocks promotion.
- Maintainers deliberately decide **when** a verified release is promoted.
  Mark its draft PR ready for review after CI succeeds; this retriggers
  verification. Review and merge manually through the protected PR path.
  Recheck the required master PR checks and release evidence before merging.

### Initial master ancestry reconciliation

The first `release/2 → master` promotion may report merge conflicts even
though `release/2` already includes the same modern dependency versions and
GitHub Actions versions. Historically `master` received two Dependabot commits,
`092c96c` (#29, Cargo dependencies) and `1fca85b` (#30, Actions upgrades),
after the common ancestor. Their changes later reached `release/2`
independently, so Git does not know those histories were reconciled.

The one-time reconciliation PR contains a **two-parent merge commit** with the
current `release/2` commit as first parent and the existing `master` commit
as second parent. The resolved tree keeps the validated `release/2` contents,
including modern Cargo dependencies and Actions, rather than reverting them to
old `master` definitions. This documentation note is the only intended file
change.

**IMPORTANT — select "Create a merge commit" in the GitHub merge-method
dropdown for the reconciliation PR into `release/2`.** Do **not** select
"Squash and merge" or "Rebase and merge". PR #117 was squash-merged on
2026-10-15: its original two-parent commit `6d9aa72` had `master` as a
parent, but the squash result `6f6e20a` has only `release/2` as a parent.
That is why promotion PR #116 still reported the same conflicts.

The follow-up reconciliation PR includes a fresh two-parent merge commit based
on that new release head. Review its GitHub commit page and confirm its parents
are the current `release/2` and `master` SHAs. **On completion verify
ancestry, not just a green merge button:**

```sh
git fetch origin master release/2
git merge-base --is-ancestor origin/master origin/release/2
```

This command must return exit code **0**. If it returns **1**, the merge was
squashed/rebased again and the promotion conflict is not fixed. Never
force-push either protected branch or bypass release checks. Once ancestry is
verified, wait for the **new exact release/2 SHA** to pass the real Arch VM
and security gates before manually promoting PR #116. The reconciliation PR
does not itself approve or merge a release into `master`.

Note: `master` currently has a GitHub ruleset requiring **linear history**,
so its eventual promotion uses a method that respects that restriction; it is
a separate operation from the **merge-commit reconciliation into release/2**.
Future promotions should also account for the master's new squash/rebase
commit in the next release branch to avoid reintroducing diverging history.

### Model-catalog ancestry reconciliation for PR #140 (2026-10-17)

After the preceding release promotion (#137), `master` at
`3fe7f4917ecd16a6f3bc1c6bc52705f4b67275e6` already includes the first
local-model-catalog tranche (#136). Meanwhile `release/2` at
`7d5ea00bfa1e6951ddd381829c8b19c7ba82eae1` independently contains
that same catalog, plus expert AI profiles, workload admission and usage
accounting, and the fault-isolated synthetic inference provider (tranches
2–4). The common ancestor predates the catalog on both paths. Git therefore
reports overlapping edits and add/add conflicts for the draft promotion
PR #140 even though the release branch supersets the catalog behavior.

Resolve the ancestry *through a separate PR into `release/2`*, without
pushing to either protected branch. Its reconciliation commit must have
the above `release/2` SHA as its **first parent**, the above `master` SHA
as its **second parent**, and a resolved tree that retains every
`release/2` runtime, schema, test, CI and documentation change. This note
is the only additional file change. Do not replace the later compute
features with the older `master` versions.

**Merge that reconciliation PR using "Create a merge commit" only.**
Squash/rebase discards the `master` ancestry; #117 demonstrates the
failure mode. Confirm afterwards that `git merge-base --is-ancestor
origin/master origin/release/2` succeeds, then re-check #140. Treat
the new exact `release/2` SHA as *unverified* until release-branch push
CI, the executed real Arch VM step, security and CodeQL pass. Keep
PR #140 in draft until explicit maintainer approval; never auto-merge it.

### One-time automation setup

A repository maintainer must configure the GitHub Actions secret
`FLOWERLY_FORWARD_BOT_TOKEN`, preferably from a dedicated bot account or
GitHub App, with **Contents: read** and **Pull requests: read/write** on this
repository. The bot must be authorized for protected source branches. GitHub
does not trigger mandatory `pull_request` workflows for PRs created with
the default `GITHUB_TOKEN`, so the workflow **fails visibly** if this secret
is missing instead of silently opening unchecked PRs.

To automatically finish adjacent-release forward merges, enable
**Settings → General → Pull Requests → Allow auto-merge**. This setting does
not weaken branch rules and does not enable promotion to `master`.
After configuring the secret, trigger `branch-forward` using a
`workflow_dispatch` on the active release branch to reconcile existing
branches. Until setup is complete, changes still require manual PRs; the
new workflows do not edit GitHub settings or store credentials.

For a local offline policy regression run:

```sh
python3 scripts/test-branch-forward.py
python3 scripts/release-gates.py validate
```

For stalled runs, inspect the automation logs and forward PR checks. Never
force-push release refs, retry an auto-merge around a red required check,
or promote an unverified candidate. A release source moving after its
successful test must pass fresh exact-SHA verification.

## Final Arch-kernel validation

The nightly Arch-kernel workflow can also be dispatched manually with
`force=true` for `master`, the latest release branch, or both. It boots the
official Arch cloud image, upgrades it fully, reboots into the installed Arch
kernel, and runs the complete Flowerly verification inside the guest.

The tagged release workflow also runs this Arch-kernel validation as a hard
pre-publication gate. A GitHub Release is not created unless the tagged source
passes inside the fully upgraded Arch guest. The manual forced path remains
useful for validating `master` and/or the active release branch before the tag
is created.

Once Flowerly has a real generation image, this substrate-level gate remains
useful but is joined by the stronger Flowerly image boot/rollback gate.

## Release gates

A release cannot be tagged if:

- CI is red,
- the final real-Arch kernel validation is red,
- the database migration path is broken,
- the boot test fails,
- the rollback test fails for generation-related releases,
- known critical data corruption exists, or
- release notes are missing.

Pre-1.0 releases may contain incomplete features, but failures must be explicit.
Do not market unfinished capabilities as already implemented; mark things
Implemented, In progress, Planned or Research as in
[architecture.md](architecture.md).

## Upgrade and rollback (0.0.3 → 0.0.4)

Verified on 2026-10-27 with real binaries built from `master` (`c7d6ef9`,
schema 8) and `release/2` (schema 13), against a database holding a Flow and
a Goal created by the old binary:

1. **Back up first**, with the service stopped:
   `systemctl stop flowerlyd && sqlite3 /var/lib/flowerly/state.db ".backup /var/lib/flowerly/pre-0.0.4.db"`.
   This uses SQLite's backup API; never copy a live database together with its WAL file by hand.
2. **Upgrade**: install the new package and start the service. Migrations
   9–13 (ownership, per-owner foreground, Generation, Compute and audit
   ownership) run once, at start. The old Flow and Goal stay visible to the
   service account. Install the new `flowerlyd.service`; the multi-user
   drop-in stays inactive until an administrator installs it.
3. **Rollback**: stop the service, restore the backup over `state.db`, remove
   `state.db-wal` and `state.db-shm`, install the previous package and start
   it. Verified: the old binary starts on the restored schema 8 with its
   data. **Anything written after the upgrade is lost** in a rollback.

Do not start an older binary on a migrated database:

- The 0.0.3 binary has no schema-version guard. On a schema-13 database it
  happens to fail closed: a migration-10 trigger rejects its General-Flow
  setup with "FOREGROUND is per owner". That refusal is an accident of the
  schema, not a guarantee.
- From 0.0.4 on, `Store::migrate` refuses any database whose recorded
  migrations are newer than the binary supports. The binary then exits with
  "database schema version N is newer than this binary supports (M); restore
  the pre-upgrade backup or install the newer release", and the database is
  left unchanged.

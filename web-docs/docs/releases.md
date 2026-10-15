# Flowerly OS — Releases

Flowerly OS uses semantic-ish pre-1.0 versioning. Pre-1.0 does **not** mean
careless: every release is tagged, described and backed by evidence, and known
limitations are stated explicitly rather than hidden.

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

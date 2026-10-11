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

Flowerly OS uses a release-branch workflow to keep `master` stable and ensure
every release inherits the full history of its predecessors:

- `master` is the integration branch. It is protected: direct pushes are
  forbidden; only merges from `release/*` branches are allowed.
- Feature development happens on `release/N` branches (e.g., `release/1`,
  `release/2`). PRs target the active release branch.
- When a release is ready, `release/N` is merged into `master` via a PR that
  passes the `release-source-check` gate (which enforces the head branch is
  `release/*`).
- The next release branch is cut from `master` with `scripts/release-cut.sh N+1`.
  This ensures `release/N+1` contains everything merged into `release/N` and
  promoted to `master`.
- A release is tagged on `master` after the merge. Use `scripts/release-promote.sh`
  to tag and build artifacts.

This model guarantees that content flows monotonically:
`release/1` → `master` → `release/2` → `master` → `release/3` → ...

Eventually releases will also provide bootable images, packages, an SBOM,
signatures and provenance metadata. Until then, releases ship source and
artifacts with checksums.

## Final Arch-kernel validation

The nightly Arch-kernel workflow can also be dispatched manually with
`force=true` for `master`, the latest release branch, or both. It boots the
official Arch cloud image, upgrades it fully, reboots into the installed Arch
kernel, and runs the complete Flowerly verification inside the guest.

For the current pre-image releases this is additional final evidence rather
than an automated tag blocker. Once Flowerly has a real generation image and
boot/rollback release gate, this VM validation should become part of the
enforced release pipeline rather than remaining a manually requested final
check.

## Release gates

A release cannot be tagged if:

- CI is red,
- the database migration path is broken,
- the boot test fails,
- the rollback test fails for generation-related releases,
- known critical data corruption exists, or
- release notes are missing.

Pre-1.0 releases may contain incomplete features, but failures must be explicit.
Do not market unfinished capabilities as already implemented; mark things
Implemented, In progress, Planned or Research as in
[architecture.md](architecture.md).

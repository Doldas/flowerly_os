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
source tag            an annotated Git tag on main
release notes         what changed, and what is now possible
build manifest        the exact inputs and toolchain used
test result summary   which suites passed, and any skipped
known limitations     incomplete or unverified behavior, stated plainly
artifact checksums    checksums of every published artifact
```

Eventually releases will also provide bootable images, packages, an SBOM,
signatures and provenance metadata. Until then, releases ship source and
artifacts with checksums.

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
[`ARCHITECTURE.md`](ARCHITECTURE.md).

# Proposed ADR: pinned Arch package inputs for Generations

**Status:** Proposed — no snapshot source or archive trust contract approved.
**Issues:** #49, #9, #48. **Scope:** prerequisites for 0.0.5.
This document is a specification, **not** an implemented package fetcher.

## Problem

Arch mirrors are rolling; rebuilding from a current mirror can resolve
different package versions even when the Flowerly source commit is fixed.
A reproducible Generation must record and reacquire the exact package
archives used to build a candidate. Reproducible **inputs** do not
automatically mean byte-identical output images.

## Acquisition options

| Source | Benefit | Risk / required proof |
| --- | --- | --- |
| Arch Linux Archive dated snapshots | Historical repository state | Snapshot retention, repository index integrity and detached package signatures |
| Internally mirrored signed package archive | Offline operation, retained exact archives | Key custody, mirror immutability, storage cost and audit requirements |
| Live Arch repository | Simple fresh installs | Mutable package selection; not acceptable for reproducible Generation promotion |

**Proposed evaluation order:** test a dated upstream Arch snapshot first,
then determine if a content-addressed internal cache is required for
retention and offline boots. This is not a trust-on-first-use exception.
Never fall back silently from a missing pinned artifact to a live mirror.

## Manifest schema proposal

The manifest is immutable once a candidate is verified. It includes:

- Manifest schema version and deterministic serialization/hash rules.
- Source commit SHA, parent Generation ID, build recipe revision.
- Snapshot timestamp and repository URL, repository database SHA256 and
  signature verification evidence.
- For each package: name, epoch/version/release, architecture, repository,
  archive URL, archive SHA256, size, package signer fingerprint and
  signature verification outcome.
- Kernel package version, kernel config digest, initramfs build inputs,
  bootloader version and root filesystem content hash.
- Toolchain versions, locale, timestamps policy, compression parameters
  and builder architecture.
- Trust-policy version and the exact signed keyring state used for
  verification, without exporting private signing material.

Packages are sorted by repository, architecture, name, version and hash;
duplicate identities with divergent hashes are an error.

## Trust and fetch protocol

1. Obtain signed repository metadata from the configured pinned source.
2. Verify the trusted keyring and signatures against explicit trust policy.
3. Resolve the full transitive package set into a frozen manifest.
4. Download by exact immutable URL and verify artifact size, SHA256 and
   package signature before making an artifact available to the builder.
5. Store verified archives by digest, never by a mutable version-only name.
6. Rebuild only from these cached verified artifacts, including offline
   mode. No live mirror fallback; missing artifacts fail closed.
7. Bind the manifest digest to the resulting Generation image and boot
   acceptance report. Do not claim exact input reproduction if any
   artifact differs.

A checksum alone identifies bytes, but does not establish the publisher's
authority. Both content hash and signature trust must be checked.

## Offline and retention policy

The active Generation, pending Generation, last verified rollback target
and release-attested artifacts are protected from automated eviction.
Garbage collection needs an explicit reachability proof and must never
remove the last bootable image. Offline installation fails with a
machine-readable missing-digest list without substituting newer packages.
Repository trust-policy/key rotation requires a recorded migration plan.

## Acceptance evidence before implementation approval

1. Freeze a package set from one dated repository snapshot.
2. Save the full manifest and verified package artifacts.
3. Deliberately advance or replace the live mirror.
4. Restore the exact artifact digests from the snapshot/cache with the
   same trusted signatures, then build a new candidate.
5. Tamper with a package, metadata, key identity or cache entry and show
   that the build fails before installation.
6. Remove one cached archive with networking disabled and verify a
   fail-closed missing-artifact result.
7. Record archive bytes, cache amplification, download time, image-build
   time, rootfs size, QEMU acceptance time and the nightly runner budget.
8. Separate input identity checks from any claim of byte-identical rootfs
   output. Record unexplained nondeterminism instead of ignoring it.

The prototype requires real Arch package signatures and actual artifact
retrieval. Unit mocks or a synthetic package manifest alone do not
resolve #49. Do not begin Generation image activation before the trust and
rollback prototypes have been accepted.

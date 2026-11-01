# Proposed ADR: pinned Arch package inputs for Generations

**Status:** Proposed — no snapshot source or archive trust contract approved.
**Issues:** #49, #9, #48. **Scope:** prerequisites for 0.0.5.
This document is a specification, **not** an implemented package fetcher.

## Implementation status

| Part | State |
| --- | --- |
| Manifest schema `flowerly.generation-inputs.v1`, canonical form and digest | Implemented: `distro-builder manifest check` |
| Content-addressed cache verification, missing/tampered artifacts fail closed | Implemented: `distro-builder manifest verify-cache` |
| Detached signature check against a supplied keyring | Implemented: `verify-cache --keyring <file>` (gpgv) |
| Trust policy: which keyring is acceptable, key expiry on historic replay | **Not decided**; without `--keyring` the report says `signature_trust: not_verified` |
| Repository database verification and transitive resolution | Not implemented |
| Fetch from a dated snapshot, real frozen package set, mirror-change demo | Not done; needs approval for network acquisition |
| Kernel, toolchain and output identity fields | Not in v1; a later schema version |

A passing `verify-cache` proves only that the cached bytes are the bytes the
manifest names. It does not establish who published them. #49 stays open
until the acceptance evidence below exists.

### v1 format

The manifest is a JSON object with `schema`, `source`
(`snapshot_url`, `snapshot_date`, `architecture`), `requested` and
`packages`. Each package has `repository`, `name`, `version`,
`architecture`, `filename`, `size`, `sha256`, `signature_sha256` and
`signer_fingerprint`. Rules the checker enforces:

- Unknown fields and any other `schema` value are rejected.
- The stored bytes must equal the canonical form: pretty-printed JSON with
  object keys sorted at every level and one final newline. The SHA-256 of
  the file is the manifest identity.
- `snapshot_url` is a plain `https://` directory URL whose path contains
  `snapshot_date` as `YYYY/MM/DD`; a live mirror URL is rejected.
- Names, versions (`[epoch:]pkgver-pkgrel`) and repositories use pacman's
  alphabets; `filename` must be exactly
  `<name>-<version>-<architecture>.pkg.tar.{zst,xz}`; `architecture` is
  `any` or the source architecture.
- Packages are strictly sorted by repository, architecture, name, version
  and SHA-256. A name may be resolved once; no two artifacts share a digest.
- `requested` is sorted, unique, contained in the resolved set, and equal
  to `distro/base-image/arch/packages.base`.

The cache layout is `<cache>/sha256/<hex digest>` for archives and detached
signatures alike. Entries must be regular files, are opened without
following symbolic links, and must match the manifest size (archives) and
digest. The JSON report lists every missing and mismatched artifact; the
command exits non-zero without fetching or substituting anything.

### Signature check

With `--keyring <file>` (a binary OpenPGP keyring, as `gpg --export`
writes it), each package whose archive and signature bytes verified is
also checked with `gpgv`. `gpgv` runs with an empty throwaway home, a
cleared environment and only that keyring; only its `--status-fd` output
is read. A package passes when the status holds exactly one `NEWSIG`,
`GOODSIG` and `VALIDSIG`, the `GOODSIG` key id matches the signer, no
rejecting keyword appears (`BADSIG`, `ERRSIG`, `EXPSIG`, `EXPKEYSIG`,
`REVKEYSIG`, `NO_PUBKEY`, `KEYEXPIRED`, `KEYREVOKED`, `SIGEXPIRED`,
`FAILURE`, `ERROR` and others), the `VALIDSIG` primary fingerprint equals
the manifest's `signer_fingerprint`, the hash is SHA-2 or SHA-3, the
signature class is a binary document, and `gpgv` exits 0. The report then
says `signature_trust: verified_against_supplied_keyring` and records
`keyring_sha256`; any problem makes it `failed` and the command non-zero.

This proves the archives were signed by the keys the manifest names, as
present in that keyring. It does not decide that the keyring is the right
one; that is the open trust policy. Open policy questions:

- **Which keyring.** Arch trusts packager keys certified by enough master
  keys (`archlinux-keyring`). Flowerly checks one named key per package.
  Policy must say how a keyring is chosen, pinned and rotated.
- **Expiry on replay.** `gpgv` evaluates key expiry and revocation at the
  current time. A manifest replayed after a packager key expired fails
  closed. Whether historic replay should accept that, and with what
  evidence, needs a decision.
- **Time of check.** Signatures are checked by path after the bytes were
  hashed. The cache is assumed to be writable only by the operator running
  the check.

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

# #49 Gate B — a real frozen, signed Arch package set (2026-11-16)

Status: **FROZEN SET PRODUCED AND INDEPENDENTLY VERIFIED; GATE B NOT CLOSED.**
Input identity only: this does not claim bit-for-bit reproducible images, and
the manifest is not yet bound to a built Generation. Network use was approved
by the owner for `https://archive.archlinux.org` only (no keyserver, no
mirror, no other host).

## What was done

1. `scripts/freeze-arch-snapshot.py freeze` read `core.db` and `extra.db` of
   the Arch Linux Archive snapshot **2026/11/09**, resolved the transitive
   closure of `distro/base-image/arch/packages.base`, downloaded every archive
   and detached signature from that snapshot tree, and wrote
   `distro/base-image/arch/inputs.manifest.json`
   (`flowerly.generation-inputs.v1`, 157 packages, 15 distinct signers,
   manifest SHA-256 `8689cd41…4d62`).
2. The independent Rust verifier, not the script, judged the result:
   `distro-builder manifest check` and `manifest verify-cache --keyring`
   report 314 artifacts verified, 0 missing, 0 mismatched, 0 signature
   problems, `verified_against_supplied_keyring` (18.8 s).
3. `scripts/arch-frozen-inputs-drill.py` ran the acceptance items.

Evidence: [evidence/0049-gate-b-frozen-set-8689cd4](evidence/0049-gate-b-frozen-set-8689cd4/)
(manifest, freeze evidence, drill report). The 739 MB archive cache is not in
the repository.

## Drill result

| Item | Result |
| --- | --- |
| Mirror moved: snapshot 2026/11/15 has 26 of the 157 packages at another version | cache still verifies, `verified_against_supplied_keyring` |
| One archive byte flipped | rejected: 1 mismatched |
| One signature byte flipped | rejected: 1 mismatched |
| Archive replaced **and** manifest re-hashed to match | rejected by the signature check: 1 signature problem |
| Signer fingerprint swapped in the manifest | rejected: 1 signature problem |
| Empty keyring | rejected: 157 signature problems |
| Manifest not in canonical form | rejected before any cache read |
| One archive removed, no network | rejected: 1 missing, nothing substituted |

The drill requires each rejection to have exactly the expected cause, so a
broken drill cannot pass as enforcement. (An earlier draft linked cache
entries with symlinks, which the verifier refuses everywhere; every case then
"failed" for the wrong reason. That was caught by the per-cause counts and
fixed with hard links.)

## Findings

- **No `.db.sig` for snapshots.** The Archive returns 404 for repository
  database signatures. The database fields are therefore bound through the
  per-package signatures: the fetched `.sig` must equal the database's
  `%PGPSIG%`, and the archive must match the database hash and the signature.
- **Trust anchor is a reviewed pin**, the SHA-256 of the snapshot's
  `archlinux-keyring` package (policy file `trust-policy.json`). It was
  produced from the snapshot database, so it is trust on first review. The
  master-key certification chain is not evaluated; 38 revoked fingerprints
  from the keyring package are refused.
- **Ambiguous virtual dependencies** (`libxtables.so`: iptables or
  iptables-legacy; `initramfs`: mkinitcpio, dracut, booster) stop the freeze.
  They are decided in `provider-choices.txt`. `initramfs` is `-` because mkosi
  builds the initrd itself; choosing mkinitcpio first would have added
  mkinitcpio, mkinitcpio-busybox and diffutils.
- **Cross-check against a real build.** The package names of the earlier real
  Arch rootfs (`target/flowerly-arch-rootfs`, built from a then-live mirror)
  equal the frozen set exactly (157 names, none extra on either side). Versions
  of that build were not pinned, which is the problem this gate removes.
- **Expiry on replay** fails closed (see the ADR); no packager key was expired
  at the time of the run.
- Size: 738,921,801 bytes of archives, 739 MB cache (signatures and keyring
  add little). The kernel `linux` is 169 MB and the firmware packages are the
  next largest, so a nightly cache needs about 0.75 GB plus the builder's
  scratch. Download time was not measured separately.

## Pinned build (same day)

`FLOWERLY_PINNED=1 scripts/build-arch-rootfs.sh` passes
`--mirror=<manifest snapshot_url>` to mkosi. The proof build ran in an Arch
container (mkosi, arch-install-scripts, cpio from the live mirrors; tooling is
not part of the frozen inputs) with the same arguments. pacman resolved 155
packages plus the requested ones from
`https://archive.archlinux.org/repos/2026/11/09`, including kernel
7.2.14-arch1-1.

| Check | Result |
| --- | --- |
| `manifest verify-rootfs` on the built rootfs | 157 installed, 0 missing, 0 unexpected, 0 version, 0 architecture, 0 unreadable |
| `manifest verify-binding` with the built `vmlinuz` | identity names the manifest; staged kernel `b1e9eaf5…325b` is the frozen one |

The earlier live-mirror rootfs failed the first check with 27 version
differences. Evidence:
[evidence/0049-gate-b-pinned-build-8689cd4](evidence/0049-gate-b-pinned-build-8689cd4/).
The identity used for the binding check carried the manifest digest from a
drill file; a Generation baked by `distro-builder slot stage` with this
rootfs is the next integration, not claimed here. The rootfs bytes are not
claimed reproducible; this is input identity.

## Gate D re-run on the pinned rootfs (same day)

`scripts/build-g5-test-image.sh` (new; the pipeline previously lived only in a
scratch directory) turns the pinned rootfs into the Gate D inputs: slots A, B
and B2 (test-only schema-drill `flowerlyd`), identities that carry the
committed manifest digest `8689cd41…4d62`, the two-slot disk with B staged,
and the kernel and `systemd-bootx64.efi` taken from the frozen packages. Each
slot is prepared with `FLOWERLY_FROZEN_INPUTS`, so none of them would have
been baked from a rootfs that is not exactly the frozen set. `verify-binding`
confirmed the staged kernel is the frozen `vmlinuz`.

The exact-SHA KVM matrix (`b05e3be`, real OVMF/KVM, harness unchanged) then
gave **16 of 16 scenarios PASS**, including the restore drill and the
exhausted-target observation (the loader again started only the target slot).
Evidence: [evidence/0049-g5-pinned-rootfs-b05e3be](evidence/0049-g5-pinned-rootfs-b05e3be/).
The initramfs is the generic mkosi initrd from the earlier run (not part of
the frozen set).

## Not done

- Kernel config, toolchain and output identity fields (a v2 schema).
- A rootfs built from the snapshot, and binding the manifest digest to a
  built Generation image and boot report (ADR-0020 step 7). The comparison
  tool exists (`distro-builder manifest verify-rootfs`; the earlier live-mirror
  rootfs fails it with 27 version differences and `openssl-3` extra), the
  pinned build does not.
- CI retention of the cache and a scheduled drill; the freeze is a manual,
  owner-approved network action.
- Evaluation of the master-key certification chain.

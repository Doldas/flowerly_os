# #49 Gate B — a real frozen, signed Arch package set (2026-11-16)

Status: **Frozen inputs independently verified and built into the Gate D fixture.**
The frozen inputs identify the packages used by the Generation fixture.
The resulting filesystem can have different bytes between builds. Network use was approved
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
  scratch. See the measured fresh acquisition below.

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

## Kernel configuration and toolchain identity

- **Kernel configuration.** `verify-binding` now also reports the build
  configuration embedded in the frozen kernel (IKCONFIG, found the way the
  kernel's `extract-ikconfig` does it): SHA-256 `e8f340e4…0731`, built with
  `gcc (GCC) 16.2.1 20260810`. It is reported only when the staged kernel is the
  frozen one. (The configuration is not a separate file in the `linux` package.)
  Evidence: `evidence/0049-gate-b-pinned-build-8689cd4/verify-binding-with-config.json`.
- **Build tooling of the proof build.** Container image
  `docker.io/library/archlinux@sha256:dfc5be7be44cf1acf3d9b3b902e3e9c8a29f97ed41e35631823cdc9c97e5d4f0`
  (created 2026-10-27), mkosi 27.1, pacman and arch-install-scripts from that
  day's live mirrors. These are recorded, **not frozen**: they decide how the
  rootfs is assembled but not which package bytes it contains, and
  `verify-rootfs` checks the result.
- **Tooling now pinned too** (`scripts/build-arch-rootfs-pinned-container.sh`,
  `distro/base-image/arch/build-tooling.json`): the container image is named by
  digest and its pacman is pointed at the manifest's snapshot, so the tools are
  the snapshot's (mkosi 27.1-1, arch-install-scripts 32-1, cpio 2.15-3,
  diffutils 3.12-2, python-pefile 2024.8.26-2, pacman 7.1.0.r9.g54d9411-3).
  A build with it passes `verify-rootfs` (157 / 0 / 0 / 0) and its kernel is
  the frozen `vmlinuz`. Evidence:
  `evidence/0049-gate-b-pinned-tooling-8689cd4`. The image is still a registry
  object (its digest is verified by podman on pull) and the keyring bootstrap
  inside the container trusts the image's own pacman keyring.
- **Cache retention** (decided 2026-11-17: runner-local, manual; manifest v2 at the next re-freeze). The policy considered: the
  cache (about 0.75 GB, content-addressed) is rebuilt by the manual, approved
  `freeze-arch-snapshot.py` network step and kept on the runner that builds the
  image; CI never fetches. A CI job would need a persistent volume or release
  asset, the keyring pin and this manifest; the drill proves a cache is
  complete without network.

## Upstream kernel source identity (2026-11-19)

The pinned manifest names the signed x86_64 package `linux-7.2.14.arch1-1`.
The archive retrieved from Arch Linux Archive matched the manifest SHA-256
`e5c8674514d6ec198b24ebc26bd282a44afa2ce445662d8958fbdc7717bf3bb7`.
Its `.BUILDINFO` names the same package version and records
`pkgbuild_sha256sum = 4c0e83cf54c98401b98383c2c7902c9c853d1372348077797143fa32171c59b5`.
That is the SHA-256 of the [Arch packaging tag's PKGBUILD](https://gitlab.archlinux.org/archlinux/packaging/packages/linux/-/blob/7.2.14.arch1-1/PKGBUILD).

| Source input declared by that recipe | SHA-256 |
| --- | --- |
| [Linux stable v7.2.14 tarball](https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.2.14.tar.xz) | `bb1b871ef95a8de10b3f7b7baf0e0d754abbf620f804b6caf860f226aa97647d` |
| [Arch v7.2.14-arch1 patch](https://github.com/archlinux/linux/releases/tag/v7.2.14-arch1) | `8048e7c6bf74e4d9195cfea2f52b47326eed35b85b6be90ab9ba1928dc0575fa` |

Arch's signed release tag points to commit
[`8ae86fbb961a2953a8883b767465417f667d8d21`](https://github.com/archlinux/linux/commit/8ae86fbb961a2953a8883b767465417f667d8d21).
Together with the `verify-binding` evidence above, this identifies the
declared source revision and recipe, embedded config and compiler, and
actual kernel bytes used in the Generation fixture.

To repeat the package-to-recipe comparison, download the package named in
the manifest and the tagged recipe as `PKGBUILD`. Compare both SHA-256
digests and extract the recipe hash from the archive without running it:

```sh
sha256sum linux-7.2.14.arch1-1-x86_64.pkg.tar.zst PKGBUILD
bsdtar -xOf linux-7.2.14.arch1-1-x86_64.pkg.tar.zst .BUILDINFO |
  sed -n '/^pkgbuild_sha256sum = /p'
```

## Repeat acquisition and KVM resource measurements (2026-11-19)

On `d6533714084ac39d1067adef0178880dec25afed`, a fresh cache populated
from the same dated snapshot took **241.225 s**. This measures downloading,
dependency resolution and signature verification together. The new
manifest matched the committed manifest byte for byte. A separate offline
`distro-builder manifest verify-cache --keyring` took **18.075 s** and
verified all 314 package and signature artifacts, with zero missing,
mismatched, or untrusted artifacts. The archive payload is 738,921,801 bytes.

On the same checkout, the digest-pinned builder image took **82.549 s**
to pull. `scripts/build-arch-rootfs-pinned-container.sh` then completed
in **301.609 s** using Docker as the local container runtime. This
includes installing the pinned tooling, acquiring the snapshot packages,
assembling the rootfs and exporting the output. Pacman reported 22.90 MiB
for the tooling transaction and 704.28 MiB for the rootfs transaction.
Mkosi reported **1.6 GiB** for the rootfs directory and the separate
`vmlinuz` was **17,248,768 bytes**. The offline verifier found exactly
157 installed packages with no differences, and the exported kernel
matched the frozen kernel SHA-256 in `verify-binding`.

The archived [pinned fixture KVM report](evidence/0049-g5-pinned-rootfs-b05e3be/acceptance.json)
contains 20 sequential boots with a verified KVM file descriptor. Their
recorded QEMU durations sum to **354.536 s**, including the deliberate
120.074 s observation of the exhausted target. The harness uses two
vCPUs and 2 GiB RAM per guest. This is guest-process time, with extra
host work for disk copying, hashing and running the fixture builder.
The measured cold acquisition, builder pull, rootfs build and QEMU
stages total about 16.5 minutes. For a nightly Gate D trial, budget 30
minutes for these and the remaining host work, plus 35 GiB of scratch
as recorded in the [Gate D review](0048-g5-gate-d-kvm-acceptance-2026-11-16.md).
This is a planning budget based on the measured stages and the earlier
scratch requirement. The existing nightly Arch workflow tests the
official cloud guest and does not schedule this fixture drill.

## Not done

- A v2 manifest schema that carries the tooling inside the manifest (it is a
  separate pinned file today).
- CI retention of the cache and a scheduled drill; the freeze is a manual,
  owner-approved network action.
- Evaluation of the master-key certification chain.

# Proposed ADR: Generation root filesystem rollback substrate

**Status:** Proposed — requires maintainer decision and kernel/VM proof.
**Issues:** #48, #9, #31. **Scope:** 0.0.5 design only.
This document does **not** implement Generation activation or rollback.

## Context

Generation metadata changes are not the same as switching a bootable
root filesystem. A reliable rollback must preserve a verifiable correspondence
between an immutable build input, its deployed root filesystem, and the boot
entry actually selected on restart. A metadata-only "rollback succeeded"
response would be incorrect if Linux is still booting the previous root.

Flowerly currently validates in an official Arch guest, not necessarily
from a Flowerly-built bootable image. The substrate choice must be proven
on a separate test image before it is accepted as a release capability.

## Candidate comparison

| Substrate | Benefits | Open risks |
| --- | --- | --- |
| btrfs read-only snapshots | Fast local snapshot creation, checksums, copy-on-write sharing | Bootloader integration, writable subvolumes, space exhaustion, snapshot promotion semantics |
| LVM thin snapshots | Filesystem-independent block snapshots, familiar tooling | Thin-pool exhaustion, device mapping and recovery complexity, filesystem coordination |
| OSTree or image-based deployments | Content-addressed inputs, distinct deployment slots | Integration cost for Arch package workflows, initramfs/bootloader and mutable state boundaries |
| A/B immutable root images | Straightforward slot identity and rollback target | Double image storage, bootloader orchestration, health state and atomic slot selection |

**Provisional direction for a prototype:** A/B root images, not a production
decision. They make it easiest to falsify a false-success claim: after a
failed activation, the next boot must identify the previous root-image hash.
The final choice depends on tested disk overhead, boot reliability and
maintenance burden. This direction must not displace current 0.0.4 work.

## Required identity and commit protocol

Each candidate needs a durable manifest with at least:

- Generation ID, parent Generation ID and source commit SHA.
- Rootfs artifact SHA256, size, filesystem format and root identifier.
- Exact kernel/initramfs hashes and selected boot entry identifier.
- Input manifest hash and trusted-signature verification result.
- State migration compatibility range; mutable state is excluded from
  automatic rollback unless an explicit reversible migration exists.

The transition is intentionally **two-phase**:

1. Stage an immutable, verified candidate in the inactive slot. Verify
   all hashes and space headroom without changing the running root.
2. Durably write a pending activation record and a one-shot boot target.
   Do not record success yet.
3. Reboot. The early-boot witness reports the **actual** root UUID/hash,
   kernel, generation and health-check result to a durable ledger.
4. Mark active only after witness and health gates agree with the pending
   candidate. On timeout or boot failure select the last verified root.
5. Keep the old slot available until a separately authorized garbage
   collection decision.

Do not trust a caller-supplied Generation ID, filesystem path or boot UUID
as authority. Verify the selected root against trusted metadata acquired
from the booted system.

## Failure and safety invariants

- Power loss before slot switch: boot remains on the previous verified root.
- Power loss after switch but before healthy witness: pending remains
  unresolved; automatic fallback must have a bounded retry count.
- Candidate fails hash, signature or space checks: no boot entry mutation.
- Missing original slot or corrupted bootloader metadata: fail closed and
  require out-of-band rescue; never claim successful rollback.
- A failed health check must not delete user data or erase the last known
  good bootable root.
- Schema migrations requiring irreversible data changes block automatic
  rollback until a compatible recovery plan is approved.
- Child process / PTY cleanup and Linux cgroup authority remain independent
  of Generation activation; no bypass of 0.0.4 kernel gates.

## Recovery, retention and ownership

Track staged bytes, unique bytes and free-space reserve *before*
activation. Garbage collection is restricted to unreferenced candidates
and must preserve the active, pending and most recent healthy roots.
A privileged boot/rootfs broker owns slot writes and boot entries; domain
services issue typed intents rather than raw mount paths or shell commands.
An operator recovery mode must be documented for broken initramfs,
storage-full, signature mismatch and two failed boots.

## Prototype acceptance (must be measured)

A dedicated Flowerly-built QEMU/KVM guest, not the official Arch test
guest alone, must demonstrate all of the following:

1. Build two distinguishable hashed root images A and B from pinned inputs.
2. Boot A and prove the witnessed root hash.
3. Stage B, activate pending state, boot B and prove witnessed new hash.
4. Force B health failure and prove automatic return to A on next boot,
   with the rollback record truthfully reflecting A.
5. Inject power loss before and after slot selection; verify recovery
   without manual database reconstruction.
6. Force full storage, tampered artifact, invalid signature, damaged boot
   metadata and interrupted install; show safe failure and no false success.
7. Measure retained bytes, creation/activation time and clean-shutdown time.
8. Save exact-SHA build inputs, QEMU version, boot logs, root witness and
   artifacts as an auditable CI record without credentials.

The proposal is **not accepted** until the maintainer reviews a real
boot/rollback run, chooses the substrate, documents its threat model and
updates #48 and #9. No runtime activation claims are authorized by this ADR.

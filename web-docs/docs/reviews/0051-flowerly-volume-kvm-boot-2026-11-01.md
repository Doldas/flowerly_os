# #51 evidence — first boot of a Flowerly-built root volume in KVM (2026-11-01)

Scope: **direct-kernel boot of the built ext4 volume**, not a partitioned
disk, bootloader or A/B slots (those are steps S4–S7 of the
[0.0.5 step 1 plan](../release-0.0.5-generations-plan.md)). It shows the
build chain produces a volume whose systemd reaches `multi-user.target` and
whose `flowerlyd` answers an IPC Ping. It is not sustained health.

## What ran

- Code: `fix/51-first-flowerly-volume-boot` at `3155fcf` on top of `a6a7833`
  (ext4 ownership fix). Release binaries from `cargo build --release --locked`
  of the same tree.
- Rootfs: `scripts/build-arch-rootfs.sh` with mkosi inside a rootless
  `podman run --privileged` Arch container (no host install, no sudo). Kernel
  `7.2.11-arch1-1`, 1.6 GiB tree owned by subordinate ids on the host.
- Then `install-flowerly-rootfs.sh` and `prepare-arch-boot.sh` through
  `podman unshare`, `build-arch-initrd.sh` in the container, and
  `build-arch-root-volume.sh` on the host (4 GiB ext4, took about 4 s; used
  `podman unshare` and passed the setuid/setgid ownership audit).
- Boot: `probe-arch-qemu-boot.py --accel kvm --timeout 90`, QEMU 10.2.1,
  `q35,accel=kvm`, `-cpu host`, 2 vCPU, 2 GiB, no network, volume in
  `snapshot=on` mode.

## Result

`accepted: true`. The serial log shows `Reached target Multi-User System` and
the `FLOWERLY_BOOT_IPC_HEALTHY` witness about 7.5 s after kernel start.

| Artifact | SHA-256 |
| --- | --- |
| volume (`flowerly-root.ext4`) | `a58d2a93190a658edbdb50a78e9930617612670951d03a01e874ffbd94b2964b` |
| kernel `vmlinuz` | `7ca8265d776950dd3cdfb8eab7ee33978d139125e09bcf2a3a4ff482b7d5728c` |
| initramfs | `58990e573de5c6d0848d103acecc9e8e6b4189cc2f548f5befad9647961da348` |
| serial log | `6ec84209f38d9f63e5e3d669afc02b7fd3e56ef6fd8f21af3e0fa0adaec218e9` |

The packages are the rolling Arch set of that day: **inputs are not pinned**
(#49), so this volume is not reproducible from the manifest yet.

## Defects found by the run (all fixed in `3155fcf`)

1. `mkfs.ext4 -d` Permission denied on subordinate-id files (fixed in
   `a6a7833`).
2. The initrd `Output=` produced `flowerly-initrd.cpio.cpio.zst`.
3. `systemd-firstboot` blocked on interactive timezone input; the unit is
   now masked in boot preparation.
4. `journal+console` prefixes the witness with `[ t] bash[pid]: `; the probe
   accepts exactly that prefix.

## Not shown

TCG (software emulation) was only observed reaching the firstboot prompt
before fix 3; the default `--accel tcg` path was not re-run end to end after
the fixes. No power-loss, failed-boot or rollback behavior, no reproducible
build, no CI run of this pipeline (it needs mkosi, a privileged container
and KVM).

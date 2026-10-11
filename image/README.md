# image

Flowerly OS image and root-filesystem build inputs live here.

The initial substrate is Arch Linux, selected by
[ADR-0015](../docs/adr/0015-arch-linux-userspace-substrate.md). Arch is an
upstream package source; Flowerly generations own testing, promotion and
rollback.

Current contents:

- `arch/` — minimal Arch rootfs inputs and the first mkosi prototype.

The directory is intentionally not presented as a finished image builder yet.
Bootable output, Flowerly package installation, initramfs/UKI generation and
KVM validation remain roadmap work. See
[docs/image-model.md](../docs/image-model.md).

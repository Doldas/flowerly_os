# Arch Linux base-image inputs

The initial Arch root filesystem scaffolding lives in `arch/`. The distro
builder can validate inputs and `just image-rootfs` can produce a\nverified Arch directory root filesystem using mkosi and pacman. A bootable\ndisk/ISO and Flowerly service installation are **not implemented**. Upstream Arch content and Flowerly generations remain separate.

# image

Produced distribution image artifacts. Building a real image is planned work
(the `just image` target is a placeholder that fails until implemented).

- `rootfs/` — minimal stable-world root filesystem definition
- `initramfs/` — early boot initramfs construction
- `installer/` — installation/partitioning tooling
- `build/` — image assembly scripts

See `docs/EVOLUTION.md` and `docs/GENERATIONS.md` for how candidate images are
booted and become reversible generations.
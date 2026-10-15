# Arch kernel system test

This suite is a heavyweight whole-project validation that runs Flowerly **inside
an actual Arch Linux virtual machine**, not merely inside an Arch container on
another distribution's kernel.

The host harness:

1. downloads the official Arch Linux cloud image from
   `geo.mirror.pkgbuild.com`;
2. verifies the published SHA256;
3. boots it with QEMU, preferring KVM and falling back to TCG;
4. runs a full `pacman -Syu`;
5. reboots into the installed Arch kernel;
6. copies the exact checked-out Flowerly worktree into the VM;
7. runs `guest-verify.sh`;
8. captures guest/kernel evidence and QEMU serial logs.

The guest verification runs formatting, Clippy, compile checks, the full Rust
test suite, a release build, docs/image-input validation, and a smoke test of
all current Flowerly daemons through `agenticctl`.

The scheduled workflow tests `master` and the highest-numbered `release/N`
branch once per night, but uses a content fingerprint so a branch is skipped
when its code/build/image inputs are unchanged since the last successful run.
A manual forced run is available for release/final validation.

This is an early executable slice of the Evolution World architecture. It does
not yet build and boot a Flowerly generation image; it validates the project on
the real rolling Arch substrate while that image pipeline is being built.
